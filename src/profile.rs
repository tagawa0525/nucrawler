//! 関心プロファイル：関心分野と重み、除外したい話題。TOML で書いて DB に取り込む。

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("failed to parse profile")]
    Parse(#[from] toml::de::Error),
    #[error("invalid profile: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(rename = "interest", default)]
    pub interests: Vec<Interest>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interest {
    pub topic: String,
    /// 0〜1。大きいほど推薦で重視する
    pub weight: f64,
    /// 採点の際に LLM へ渡す補足
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 関心分野の数の上限。
pub const MAX_INTERESTS: usize = 50;
/// 推薦しない話題の数の上限。
pub const MAX_EXCLUDES: usize = 50;
/// 関心分野の名前と推薦しない話題の長さの上限（文字数）。
pub const MAX_TEXT_CHARS: usize = 50;
/// 関心分野の補足（note）の長さの上限（文字数）。
pub const MAX_NOTE_CHARS: usize = 500;

/// TOML を読み、値を検証する（`validate`）。
pub fn parse(text: &str) -> Result<Profile, ProfileError> {
    let profile: Profile = toml::from_str(text)?;
    validate(&profile)?;
    Ok(profile)
}

/// 値を検証する（重みは 0〜1、topic と exclude は空でなく重複しない、どの文字列も制御文字を含まない、
/// 件数と長さは上限まで、重みが正の関心分野が 1 つはある）。
pub fn validate(profile: &Profile) -> Result<(), ProfileError> {
    // 好みのベクトルを作る呼び出しの量を抑える
    for (what, count, max) in [
        ("interests", profile.interests.len(), MAX_INTERESTS),
        ("excludes", profile.exclude.len(), MAX_EXCLUDES),
    ] {
        if count > max {
            return Err(ProfileError::Invalid(format!(
                "at most {max} {what}, got {count}"
            )));
        }
    }
    let lengths = profile
        .interests
        .iter()
        .map(|i| (&i.topic, MAX_TEXT_CHARS))
        .chain(
            profile
                .interests
                .iter()
                .filter_map(|i| i.note.as_ref().map(|n| (n, MAX_NOTE_CHARS))),
        )
        .chain(profile.exclude.iter().map(|e| (e, MAX_TEXT_CHARS)));
    for (text, max) in lengths {
        if text.chars().count() > max {
            return Err(ProfileError::Invalid(format!(
                "{text:?} is longer than {max} characters"
            )));
        }
    }
    // 案は LLM が作って端末に表示するので、エスケープシーケンスや改行を通さない
    let texts = profile
        .interests
        .iter()
        .flat_map(|i| std::iter::once(&i.topic).chain(i.note.as_ref()))
        .chain(&profile.exclude);
    for text in texts {
        if text.chars().any(char::is_control) {
            return Err(ProfileError::Invalid(format!(
                "{text:?} must not contain control characters"
            )));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for i in &profile.interests {
        if i.topic.trim().is_empty() {
            return Err(ProfileError::Invalid(
                "interest topic must not be empty".into(),
            ));
        }
        if !seen.insert(i.topic.as_str()) {
            return Err(ProfileError::Invalid(format!(
                "duplicate interest topic {:?}",
                i.topic
            )));
        }
        if !(i.weight.is_finite() && (0.0..=1.0).contains(&i.weight)) {
            return Err(ProfileError::Invalid(format!(
                "weight of {:?} must be between 0 and 1, got {}",
                i.topic, i.weight
            )));
        }
    }
    let mut excluded = std::collections::HashSet::new();
    for e in &profile.exclude {
        if e.trim().is_empty() {
            return Err(ProfileError::Invalid(
                "exclude must not contain an empty topic".into(),
            ));
        }
        if !excluded.insert(e.as_str()) {
            return Err(ProfileError::Invalid(format!("duplicate exclude {e:?}")));
        }
    }
    // 重みが正の関心分野が無ければ、どの記事にも関心の点が付かない
    if !profile.interests.iter().any(|i| i.weight > 0.0) {
        return Err(ProfileError::Invalid(
            "at least one interest needs a positive weight".into(),
        ));
    }
    Ok(())
}

pub fn to_toml(profile: &Profile) -> String {
    toml::to_string(profile).expect("a profile is plain data")
}

/// 現行のプロファイルから案への変更の 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Added {
        topic: String,
        weight: f64,
        note: Option<String>,
    },
    Removed {
        topic: String,
    },
    Weight {
        topic: String,
        from: f64,
        to: f64,
    },
    Note {
        topic: String,
        from: Option<String>,
        to: Option<String>,
    },
    ExcludeAdded(String),
    ExcludeRemoved(String),
}

/// 現行（`from`）と案（`to`）の差分。分野は案の順、削除は現行の順、除外は追加・削除の順に並べる。
pub fn diff(from: &Profile, to: &Profile) -> Vec<Change> {
    let find = |p: &'_ Profile, topic: &str| p.interests.iter().find(|i| i.topic == topic).cloned();
    let mut changes = Vec::new();
    for new in &to.interests {
        let Some(old) = find(from, &new.topic) else {
            changes.push(Change::Added {
                topic: new.topic.clone(),
                weight: new.weight,
                note: new.note.clone(),
            });
            continue;
        };
        // 空の note は無いのと同じ（suggest の案は空の note を無しにする）
        let blank = |n: &Option<String>| n.as_deref().is_none_or(|n| n.trim().is_empty());
        if old.note != new.note && !(blank(&old.note) && blank(&new.note)) {
            changes.push(Change::Note {
                topic: new.topic.clone(),
                from: old.note,
                to: new.note.clone(),
            });
        }
        if old.weight != new.weight {
            changes.push(Change::Weight {
                topic: new.topic.clone(),
                from: old.weight,
                to: new.weight,
            });
        }
    }
    for old in &from.interests {
        if find(to, &old.topic).is_none() {
            changes.push(Change::Removed {
                topic: old.topic.clone(),
            });
        }
    }
    for e in &to.exclude {
        if !from.exclude.contains(e) {
            changes.push(Change::ExcludeAdded(e.clone()));
        }
    }
    for e in &from.exclude {
        if !to.exclude.contains(e) {
            changes.push(Change::ExcludeRemoved(e.clone()));
        }
    }
    changes
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let note = |n: &Option<String>| n.clone().unwrap_or_else(|| "(none)".into());
        match self {
            Self::Added {
                topic,
                weight,
                note: None,
            } => write!(f, "add {topic} (weight {weight:?})"),
            Self::Added {
                topic,
                weight,
                note: Some(note),
            } => write!(f, "add {topic} (weight {weight:?}, note {note})"),
            Self::Removed { topic } => write!(f, "remove {topic}"),
            Self::Weight { topic, from, to } => write!(f, "weight {topic}: {from:?} → {to:?}"),
            Self::Note { topic, from, to } => {
                write!(f, "note {topic}: {} → {}", note(from), note(to))
            }
            Self::ExcludeAdded(e) => write!(f, "exclude + {e}"),
            Self::ExcludeRemoved(e) => write!(f, "exclude - {e}"),
        }
    }
}

/// 内容から決まるハッシュ（16 進 16 桁）。採点はこの値ごとに記録するので、内容が変われば
/// 採点し直しの対象になる。Rust のバージョンで値が変わらないよう FNV-1a を使う。
pub fn hash(profile: &Profile) -> String {
    let canonical = serde_json::to_string(profile).expect("a profile is plain data");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.bytes() {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interest(topic: &str, weight: f64, note: Option<&str>) -> Interest {
        Interest {
            topic: topic.into(),
            weight,
            note: note.map(Into::into),
        }
    }

    #[test]
    fn diff_lists_every_kind_of_change() {
        let from = Profile {
            interests: vec![
                interest("規制・審査", 1.0, Some("再稼働審査")),
                interest("燃料", 0.9, None),
                interest("廃止措置", 0.4, None),
            ],
            exclude: vec!["核兵器".into(), "核融合".into()],
        };
        let to = Profile {
            interests: vec![
                interest("規制・審査", 1.0, Some("再稼働審査、検査制度")),
                interest("燃料", 0.7, None),
                interest("SMR", 0.5, Some("BWRX-300")),
            ],
            exclude: vec!["核兵器".into(), "電力市場".into()],
        };
        let changes = diff(&from, &to);
        assert_eq!(
            changes,
            [
                Change::Note {
                    topic: "規制・審査".into(),
                    from: Some("再稼働審査".into()),
                    to: Some("再稼働審査、検査制度".into()),
                },
                Change::Weight {
                    topic: "燃料".into(),
                    from: 0.9,
                    to: 0.7,
                },
                Change::Added {
                    topic: "SMR".into(),
                    weight: 0.5,
                    note: Some("BWRX-300".into()),
                },
                Change::Removed {
                    topic: "廃止措置".into(),
                },
                Change::ExcludeAdded("電力市場".into()),
                Change::ExcludeRemoved("核融合".into()),
            ]
        );
        let text: Vec<String> = changes.iter().map(ToString::to_string).collect();
        assert_eq!(
            text,
            [
                "note 規制・審査: 再稼働審査 → 再稼働審査、検査制度",
                "weight 燃料: 0.9 → 0.7",
                "add SMR (weight 0.5, note BWRX-300)",
                "remove 廃止措置",
                "exclude + 電力市場",
                "exclude - 核融合",
            ]
        );
        assert!(diff(&from, &from).is_empty());
    }

    /// 空の note と note 無しは同じ（suggest の案は空の note を無しにする）。
    #[test]
    fn diff_treats_an_empty_note_as_none() {
        let from = Profile {
            interests: vec![interest("燃料", 0.9, Some(" "))],
            exclude: vec![],
        };
        let to = Profile {
            interests: vec![interest("燃料", 0.9, None)],
            exclude: vec![],
        };
        assert!(diff(&from, &to).is_empty());
    }

    fn example() -> Profile {
        parse(include_str!("../examples/profile.toml")).unwrap()
    }

    #[test]
    fn parses_example_profile() {
        let p = example();
        let weights: Vec<(&str, f64)> = p
            .interests
            .iter()
            .map(|i| (i.topic.as_str(), i.weight))
            .take(4)
            .collect();
        assert_eq!(
            weights,
            [
                ("規制・審査", 1.0),
                ("燃料", 0.9),
                ("高経年化", 0.8),
                ("安全解析", 0.7)
            ]
        );
        assert_eq!(p.exclude, ["核兵器", "核融合"]);
    }

    #[test]
    fn rejects_invalid_profiles() {
        for (toml, needle) in [
            ("[[interest]]\ntopic = \"a\"\nweight = 1.5\n", "weight"),
            ("[[interest]]\ntopic = \"a\"\nweight = -0.1\n", "weight"),
            ("[[interest]]\ntopic = \"\"\nweight = 0.5\n", "topic"),
            (
                "[[interest]]\ntopic = \"a\"\nweight = 0.5\n[[interest]]\ntopic = \"a\"\nweight = 0.2\n",
                "duplicate",
            ),
            ("exclude = [\" \"]\n", "exclude"),
            ("exclude = [\"核融合\", \"核融合\"]\n", "duplicate exclude"),
            // 端末に表示するので、制御文字（改行を含む）は受け付けない
            (
                "[[interest]]\ntopic = \"a\\u001b[2J\"\nweight = 0.5\n",
                "control",
            ),
            (
                "[[interest]]\ntopic = \"a\"\nweight = 0.5\nnote = \"x\\ny\"\n",
                "control",
            ),
            ("exclude = [\"a\\tb\"]\n", "control"),
        ] {
            let err = parse(toml).unwrap_err();
            assert!(
                matches!(&err, ProfileError::Invalid(m) if m.contains(needle)),
                "{toml}: {err}"
            );
        }
        assert!(matches!(
            parse("bogus = 1").unwrap_err(),
            ProfileError::Parse(_)
        ));
    }

    /// 好みのベクトルを作る量を抑えるため、件数と長さに上限を置く。重みが正の関心分野が 1 つも無ければ、
    /// どの記事にも関心の点が付かないので誤りにする。
    #[test]
    fn limits_sizes_and_needs_a_positive_interest() {
        let interests = |n: usize, weight: f64| {
            (0..n)
                .map(|i| format!("[[interest]]\ntopic = \"t{i}\"\nweight = {weight}\n"))
                .collect::<String>()
        };
        let excludes = |n: usize| {
            let list: Vec<String> = (0..n).map(|i| format!("\"x{i}\"")).collect();
            format!("exclude = [{}]\n", list.join(", "))
        };
        let topic = |len: usize| {
            format!(
                "[[interest]]\ntopic = \"{}\"\nweight = 1.0\n",
                "あ".repeat(len)
            )
        };
        let note = |len: usize| {
            format!(
                "[[interest]]\ntopic = \"t\"\nweight = 1.0\nnote = \"{}\"\n",
                "あ".repeat(len)
            )
        };
        // exclude は [[interest]] より前に書く（後に書くと、その関心分野の項目になる）
        let exclude_len = |len: usize| {
            format!(
                "exclude = [\"{}\"]\n{}",
                "あ".repeat(len),
                interests(1, 1.0)
            )
        };
        for ok in [
            format!(
                "{}{}",
                excludes(MAX_EXCLUDES),
                interests(MAX_INTERESTS, 1.0)
            ),
            topic(MAX_TEXT_CHARS),
            note(MAX_NOTE_CHARS),
            exclude_len(MAX_TEXT_CHARS),
            format!(
                "{}{}",
                interests(1, 0.0),
                interests(1, 0.4).replace("t0", "u0")
            ),
        ] {
            assert!(parse(&ok).is_ok(), "{ok}");
        }
        for (bad, needle) in [
            (interests(MAX_INTERESTS + 1, 1.0), "interests"),
            (
                format!("{}{}", excludes(MAX_EXCLUDES + 1), interests(1, 1.0)),
                "excludes",
            ),
            (topic(MAX_TEXT_CHARS + 1), "characters"),
            (note(MAX_NOTE_CHARS + 1), "characters"),
            (exclude_len(MAX_TEXT_CHARS + 1), "characters"),
            (interests(2, 0.0), "positive weight"),
            (excludes(1), "positive weight"),
            (String::new(), "positive weight"),
        ] {
            let err = parse(&bad).unwrap_err();
            assert!(
                matches!(&err, ProfileError::Invalid(m) if m.contains(needle)),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn toml_round_trips() {
        let p = example();
        assert_eq!(parse(&to_toml(&p)).unwrap(), p);
    }

    #[test]
    fn hash_is_stable_and_tracks_content() {
        let p = example();
        let h = hash(&p);
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash(&p.clone()), h);
        let mut changed = p.clone();
        changed.interests[0].weight = 0.95;
        assert_ne!(hash(&changed), h);
        // 固定値：実装が変わって既存の採点がすべて無効にならないよう、値そのものを固定する
        let fixed = Profile {
            interests: vec![Interest {
                topic: "規制".into(),
                weight: 1.0,
                note: None,
            }],
            exclude: vec![],
        };
        assert_eq!(hash(&fixed), hash(&parse(&to_toml(&fixed)).unwrap()));
        assert_eq!(hash(&fixed), FIXED_HASH);
    }

    /// `hash_is_stable_and_tracks_content` の固定値の期待値。
    /// 入力は `{"interest":[{"topic":"規制","weight":1.0}],"exclude":[]}`（Python で計算）。
    const FIXED_HASH: &str = "d30b50bb220fd4e1";
}
