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

/// TOML を読み、値を検証する（重みは 0〜1、topic は空でなく重複しない）。
pub fn parse(text: &str) -> Result<Profile, ProfileError> {
    let profile: Profile = toml::from_str(text)?;
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
    Ok(profile)
}

pub fn to_toml(profile: &Profile) -> String {
    toml::to_string(profile).expect("a profile is plain data")
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
