//! 同じ報道の候補選びとグループ作り（I/O なし）。
//!
//! 候補は、日本語の見出しと要約の文字 bigram の TF-IDF コサインで選ぶ。言語をまたいでも比べられるよう、
//! 英語の記事も要約（日本語）で比べる。候補が同じ報道かどうかは LLM が判定し、same の組をつないで
//! グループにする。

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};

/// 候補にする記事の日時（公開、無ければ取得）の差の上限（日）。同じ出来事でも、
/// 報じるのが 2 週間ほど遅れるソースがある。
pub const WINDOW_DAYS: i64 = 21;
/// 候補にする類似度の下限。手元の DB では、同じ報道の組が 0.18 以上に出た。
pub const MIN_SIMILARITY: f64 = 0.15;
/// 対象と別のソースから選ぶ候補の単位（グループか単独の記事）の数の上限。
pub const MAX_OTHER_SOURCE: usize = 5;
/// 対象と同じソースから選ぶ候補の単位の数の上限（連番の発表が候補を占めないように）。
pub const MAX_SAME_SOURCE: usize = 3;
/// グループの記事数の上限。別の出来事どうしが鎖状につながるのを止める。
pub const MAX_STORY_SIZE: usize = 8;

/// 比べる記事。
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub article_id: i64,
    pub source_id: String,
    pub at: DateTime<Utc>,
    /// 日本語の見出しと要約
    pub text: String,
}

/// 候補の単位。既存のグループは、まとめて 1 つの候補にする。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// 単独の記事ならその記事の ID、グループならグループの ID（`story_id`）
    pub id: i64,
    /// グループの記事（単独なら 1 件）。グループならプールの外の記事も含む
    pub members: Vec<i64>,
    /// 対象との類似度（グループなら記事の最大値）
    pub similarity: f64,
}

/// same の組。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    pub a: i64,
    pub b: i64,
    pub similarity: f64,
}

/// 全角の英数・記号を半角にし、小文字にして、空白と記号を除く。
pub fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

type Vector = HashMap<(char, char), f64>;

/// 文字 bigram の出現回数。
fn bigrams(text: &str) -> HashMap<(char, char), usize> {
    let chars: Vec<char> = normalize(text).chars().collect();
    let mut counts = HashMap::new();
    for pair in chars.windows(2) {
        *counts.entry((pair[0], pair[1])).or_insert(0) += 1;
    }
    counts
}

/// プールの記事の TF-IDF ベクトル（長さ 1 に揃える）。IDF はプールの中で数える。
#[derive(Debug)]
pub struct Index {
    docs: Vec<(Doc, Vector)>,
    by_id: HashMap<i64, usize>,
}

impl Index {
    pub fn new(docs: Vec<Doc>) -> Index {
        let counts: Vec<_> = docs.iter().map(|d| bigrams(&d.text)).collect();
        let mut df: HashMap<(char, char), usize> = HashMap::new();
        for c in &counts {
            for &g in c.keys() {
                *df.entry(g).or_insert(0) += 1;
            }
        }
        let n = docs.len() as f64;
        let docs: Vec<(Doc, Vector)> = docs
            .into_iter()
            .zip(counts)
            .map(|(doc, counts)| {
                let mut v: Vector = counts
                    .into_iter()
                    .map(|(g, tf)| (g, tf as f64 * (n / df[&g] as f64).ln()))
                    .collect();
                let norm = v.values().map(|x| x * x).sum::<f64>().sqrt();
                if norm > 0.0 {
                    v.values_mut().for_each(|x| *x /= norm);
                }
                (doc, v)
            })
            .collect();
        let by_id = docs
            .iter()
            .enumerate()
            .map(|(i, (d, _))| (d.article_id, i))
            .collect();
        Index { docs, by_id }
    }

    /// 2 件の記事の類似度（どちらかがプールに無ければ `None`）。
    pub fn similarity(&self, a: i64, b: i64) -> Option<f64> {
        let (_, va) = &self.docs[*self.by_id.get(&a)?];
        let (_, vb) = &self.docs[*self.by_id.get(&b)?];
        Some(cosine(va, vb))
    }

    /// `target` の候補を、類似度の高い順に返す。`stories` は記事からグループの ID への対応。
    /// 対象と同じグループの記事は候補にしない。
    pub fn candidates(&self, target: i64, stories: &HashMap<i64, i64>) -> Vec<Candidate> {
        let Some(&t) = self.by_id.get(&target) else {
            return Vec::new();
        };
        let (target_doc, target_vec) = &self.docs[t];
        let own_story = stories.get(&target);
        let window = chrono::Duration::days(WINDOW_DAYS);
        // 候補の単位ごとに、類似度の最大値と、対象と別のソースの記事があるか
        let mut units: BTreeMap<i64, (f64, bool)> = BTreeMap::new();
        for (doc, v) in &self.docs {
            if doc.article_id == target
                || (doc.at - target_doc.at).abs() > window
                || (own_story.is_some() && stories.get(&doc.article_id) == own_story)
            {
                continue;
            }
            let similarity = cosine(target_vec, v);
            if similarity < MIN_SIMILARITY {
                continue;
            }
            let unit = stories
                .get(&doc.article_id)
                .copied()
                .unwrap_or(doc.article_id);
            let entry = units.entry(unit).or_insert((similarity, false));
            entry.0 = entry.0.max(similarity);
            entry.1 |= doc.source_id != target_doc.source_id;
        }
        let mut units: Vec<(i64, f64, bool)> = units
            .into_iter()
            .map(|(id, (s, other))| (id, s, other))
            .collect();
        units.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let (mut other, mut same) = (0, 0);
        units
            .into_iter()
            .filter(|&(_, _, other_source)| {
                let count = if other_source { &mut other } else { &mut same };
                let cap = if other_source {
                    MAX_OTHER_SOURCE
                } else {
                    MAX_SAME_SOURCE
                };
                *count += 1;
                *count <= cap
            })
            .map(|(id, similarity, _)| Candidate {
                id,
                members: Self::members(id, stories),
                similarity,
            })
            .collect()
    }

    /// 候補の単位の記事（ID の順）。グループならプールの外の記事も含めた全員、グループでなければ
    /// その記事だけ。
    fn members(unit: i64, stories: &HashMap<i64, i64>) -> Vec<i64> {
        let mut members: Vec<i64> = stories
            .iter()
            .filter(|(_, story)| **story == unit)
            .map(|(id, _)| *id)
            .collect();
        if members.is_empty() {
            members.push(unit);
        }
        members.sort_unstable();
        members
    }
}

fn cosine(a: &Vector, b: &Vector) -> f64 {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small
        .iter()
        .filter_map(|(g, x)| large.get(g).map(|y| x * y))
        .sum()
}

/// same の組をつないだグループ（2 件以上）の、記事とグループの ID（最小の記事 ID）の対応と、
/// つなぐとグループが `max_size` を超えるので捨てた組。組は類似度の高い順につなぐ。
pub fn components(edges: &[Edge], max_size: usize) -> (BTreeMap<i64, i64>, Vec<Edge>) {
    let mut sorted = edges.to_vec();
    sorted.sort_by(|x, y| {
        y.similarity
            .total_cmp(&x.similarity)
            .then((x.a, x.b).cmp(&(y.a, y.b)))
    });
    // 素集合：親と、根ならその集合の大きさ
    let mut parent: HashMap<i64, i64> = HashMap::new();
    let mut size: HashMap<i64, usize> = HashMap::new();
    fn root(parent: &mut HashMap<i64, i64>, x: i64) -> i64 {
        let p = *parent.entry(x).or_insert(x);
        if p == x {
            return x;
        }
        let r = root(parent, p);
        parent.insert(x, r);
        r
    }
    let mut rejected = Vec::new();
    for e in sorted {
        let (ra, rb) = (root(&mut parent, e.a), root(&mut parent, e.b));
        if ra == rb {
            continue;
        }
        let (sa, sb) = (*size.get(&ra).unwrap_or(&1), *size.get(&rb).unwrap_or(&1));
        if sa + sb > max_size {
            rejected.push(e);
            continue;
        }
        parent.insert(rb, ra);
        size.insert(ra, sa + sb);
    }
    let ids: Vec<i64> = parent.keys().copied().collect();
    let mut groups: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for id in ids {
        let r = root(&mut parent, id);
        groups.entry(r).or_default().push(id);
    }
    let mut stories = BTreeMap::new();
    for members in groups.into_values().filter(|m| m.len() >= 2) {
        let story_id = *members.iter().min().unwrap_or(&0);
        stories.extend(members.into_iter().map(|m| (m, story_id)));
    }
    (stories, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-09-{day:02}T00:00:00Z"))
            .unwrap()
            .to_utc()
    }

    fn doc(id: i64, source: &str, day: u32, text: &str) -> Doc {
        Doc {
            article_id: id,
            source_id: source.into(),
            at: at(day),
            text: text.into(),
        }
    }

    /// 無関係な記事。IDF が効くよう、プールを水増しする。
    fn filler(from: i64) -> Vec<Doc> {
        [
            "九州電力、玄海原子力発電所3号機の定期検査を開始",
            "関西電力、高浜発電所の防災業務計画を修正",
            "IAEA事務局長、ウクライナ情勢について声明",
            "米エネルギー省、次世代炉の燃料供給に資金",
            "英国、核融合の実証炉計画で企業を選定",
        ]
        .iter()
        .enumerate()
        .map(|(i, t)| doc(from + i as i64, &format!("f{i}"), 15, t))
        .collect()
    }

    fn ids(cs: &[Candidate]) -> Vec<i64> {
        cs.iter().map(|c| c.id).collect()
    }

    #[test]
    fn normalize_folds_width_and_case_and_drops_symbols() {
        assert_eq!(normalize("ＳＭＲ、Ｘ-energy　（ＰＷＲ）"), "smrxenergypwr");
        assert_eq!(normalize("浜岡3・4号機"), "浜岡34号機");
    }

    #[test]
    fn similar_reports_become_candidates() {
        let mut docs = vec![
            doc(
                1,
                "wnn",
                15,
                "欧州投資銀行、初のSMR向け融資をフィンランドのSteady Energyに供与",
            ),
            doc(2, "jaif", 29, "欧州投資銀行、フィンランドのSMR開発に初融資"),
            // 窓（±21 日）の外
            doc(3, "ans", 1, "欧州投資銀行、フィンランドのSMR開発に初融資"),
        ];
        docs.extend(filler(100));
        let index = Index::new(docs);
        let cs = index.candidates(2, &HashMap::new());
        assert_eq!(ids(&cs), [1]);
        assert_eq!(cs[0].members, [1]);
        assert!(cs[0].similarity >= MIN_SIMILARITY, "{cs:?}");
        assert_eq!(index.similarity(1, 2), Some(cs[0].similarity));
        // 似ていない記事は候補にならない
        assert!(index.candidates(100, &HashMap::new()).is_empty());
    }

    /// どの記事にもある bigram も重みを 0 にしない（2 件だけのプールでも、同じ文は候補になる）。
    #[test]
    fn identical_reports_in_a_tiny_pool_are_candidates() {
        let text = "欧州投資銀行、フィンランドのSMR開発に初融資";
        let index = Index::new(vec![doc(1, "wnn", 20, text), doc(2, "jaif", 20, text)]);
        let cs = index.candidates(2, &HashMap::new());
        assert_eq!(ids(&cs), [1]);
        assert!((cs[0].similarity - 1.0).abs() < 1e-9, "{cs:?}");
    }

    #[test]
    fn caps_candidates_from_the_same_source() {
        let mut docs: Vec<Doc> = (1..=6)
            .map(|n| {
                doc(
                    n,
                    "iaea",
                    20,
                    &format!("第{}報 ウクライナ情勢に関するIAEA事務局長声明", 360 + n),
                )
            })
            .collect();
        docs.push(doc(7, "wnn", 20, "ウクライナ情勢に関するIAEA事務局長声明"));
        docs.extend(filler(100));
        let index = Index::new(docs);
        let cs = index.candidates(1, &HashMap::new());
        let same_source = cs.iter().filter(|c| c.id != 7).count();
        assert_eq!(same_source, MAX_SAME_SOURCE, "{cs:?}");
        assert!(ids(&cs).contains(&7), "{cs:?}");
        // 類似度の高い順
        assert!(cs.windows(2).all(|w| w[0].similarity >= w[1].similarity));
    }

    /// 既存のグループは 1 つの候補にまとめ、別ソースの枠を 1 つしか使わない。
    /// 対象と同じグループの記事は候補にしない。
    #[test]
    fn counts_candidates_by_story() {
        let text = "カメコ、グローバル・レーザー・エンリッチメントと全量引取契約を締結";
        let mut docs: Vec<Doc> = (1..=6)
            .map(|n| doc(n, &format!("s{n}"), 20, text))
            .collect();
        docs.push(doc(7, "t", 20, text));
        docs.push(doc(8, "u", 20, text));
        docs.extend(filler(100));
        let index = Index::new(docs);
        let stories: HashMap<i64, i64> = (1..=6).map(|n| (n, 1)).collect();
        let cs = index.candidates(7, &stories);
        assert_eq!(ids(&cs), [1, 8], "{cs:?}");
        assert_eq!(cs[0].members, [1, 2, 3, 4, 5, 6]);
        // 対象のグループは除く
        let cs = index.candidates(3, &stories);
        assert_eq!(ids(&cs), [7, 8], "{cs:?}");
    }

    /// グループの記事は、類似度の期間やプールの外にいても候補の記事に並べる（グループの全員と比べる）。
    #[test]
    fn story_candidates_list_members_outside_the_pool() {
        let text = "カメコ、グローバル・レーザー・エンリッチメントと全量引取契約を締結";
        let mut docs = vec![doc(1, "a", 20, text), doc(2, "b", 20, text)];
        docs.extend(filler(100));
        let index = Index::new(docs);
        // 記事 50 はプールに無いが、記事 1 と同じグループ
        let stories: HashMap<i64, i64> = [(1, 1), (50, 1)].into();
        let cs = index.candidates(2, &stories);
        assert_eq!(ids(&cs), [1]);
        assert_eq!(cs[0].members, [1, 50]);
    }

    #[test]
    fn components_join_edges_transitively() {
        let e = |a, b| Edge {
            a,
            b,
            similarity: 0.5,
        };
        let (stories, rejected) = components(&[e(5, 3), e(3, 9), e(20, 21)], MAX_STORY_SIZE);
        assert_eq!(
            stories.into_iter().collect::<Vec<_>>(),
            [(3, 3), (5, 3), (9, 3), (20, 20), (21, 20)]
        );
        assert!(rejected.is_empty());
    }

    /// 類似度の高い組からつなぎ、グループが上限を超える組は捨てる。
    #[test]
    fn components_cap_the_story_size() {
        let e = |a, b, similarity| Edge { a, b, similarity };
        let (stories, rejected) = components(&[e(1, 2, 0.9), e(2, 3, 0.2), e(3, 4, 0.8)], 3);
        assert_eq!(
            stories.into_iter().collect::<Vec<_>>(),
            [(1, 1), (2, 1), (3, 3), (4, 3)]
        );
        assert_eq!(rejected, [e(2, 3, 0.2)]);
    }
}
