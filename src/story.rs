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
    /// グループの記事（単独なら 1 件）。プールにあるものだけ
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
    todo!("{text}")
}

/// プールの記事の TF-IDF ベクトル。IDF はプールの中で数える。
#[derive(Debug)]
pub struct Index {}

impl Index {
    pub fn new(docs: Vec<Doc>) -> Index {
        todo!("{docs:?}")
    }

    /// 2 件の記事の類似度（どちらかがプールに無ければ `None`）。
    pub fn similarity(&self, a: i64, b: i64) -> Option<f64> {
        todo!("{a} {b}")
    }

    /// `target` の候補を、類似度の高い順に返す。`stories` は記事からグループの ID への対応。
    /// 対象と同じグループの記事は候補にしない。
    pub fn candidates(&self, target: i64, stories: &HashMap<i64, i64>) -> Vec<Candidate> {
        todo!("{target} {stories:?}")
    }
}

/// same の組をつないだグループ（2 件以上）の、記事とグループの ID（最小の記事 ID）の対応と、
/// つなぐとグループが `max_size` を超えるので捨てた組。組は類似度の高い順につなぐ。
pub fn components(edges: &[Edge], max_size: usize) -> (BTreeMap<i64, i64>, Vec<Edge>) {
    todo!("{edges:?} {max_size}")
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
