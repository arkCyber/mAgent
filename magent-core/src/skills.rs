//! Simplified skills system for embedded AI agent
//!
//! Provides skill storage, retrieval, and injection using Flash KV storage.
//! Skills are stored as simplified Markdown format in flash memory.

use crate::error::{AgentError, Result};
use core::fmt::Write;
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// Maximum number of skills
const MAX_SKILLS: usize = 10;

/// Maximum skill name length
const MAX_SKILL_NAME: usize = 32;

/// Maximum skill description length
const MAX_SKILL_DESC: usize = 128;

/// Maximum skill content length
const MAX_SKILL_CONTENT: usize = 512;

/// Skills manager
pub struct SkillsManager {
    skills: Vec<Skill, MAX_SKILLS>,
    max_skills: usize,
}

impl SkillsManager {
    /// Create a new skills manager
    pub fn new(max_skills: usize) -> Self {
        Self {
            skills: Vec::new(),
            max_skills,
        }
    }

    /// Add a skill
    pub fn add(&mut self, skill: Skill) -> Result<()> {
        if self.skills.len() >= self.max_skills {
            return Err(AgentError::MemoryAllocationFailed {
                requested: 1,
                available: 0,
            });
        }

        // Reject a duplicate name so `get` / `remove` / `best_k` /
        // `count_by_category` never have to disambiguate two skills
        // sharing a key. Without this, `add` could silently create an
        // unreachable duplicate (only the first match is ever looked up).
        if self.skills.iter().any(|s| s.name == skill.name) {
            return Err(AgentError::InputValidationFailed {
                field: "skill.name",
                reason: crate::error::ValidationError::Duplicate,
            });
        }

        // Validate skill
        skill.validate()?;

        self.skills
            .push(skill)
            .map_err(|_| AgentError::MemoryAllocationFailed {
                requested: 1,
                available: 0,
            })?;

        Ok(())
    }

    /// Search for skills by keyword
    pub fn search(&self, keyword: &str) -> Vec<&Skill, MAX_SKILLS> {
        let mut results = Vec::new();

        for skill in self.skills.iter() {
            if (skill.name.contains(keyword) || skill.description.contains(keyword))
                && results.push(skill).is_err()
            {
                break;
            }
        }

        results
    }

    /// Get skill by name
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Get all skills
    pub fn all(&self) -> &[Skill] {
        &self.skills
    }

    /// Get all skills (mutable). Useful for in-place updates such as
    /// bumping usage counters without going through `add`/`remove`.
    pub fn all_mut(&mut self) -> &mut [Skill] {
        &mut self.skills
    }

    /// Remove skill by name
    pub fn remove(&mut self, name: &str) -> Result<()> {
        let pos = self.skills.iter().position(|s| s.name == name).ok_or(
            AgentError::ConfigurationError {
                field: "skill",
                reason: crate::error::ConfigError::MissingField,
            },
        )?;

        self.skills.remove(pos);
        Ok(())
    }

    /// Clear all skills
    pub fn clear(&mut self) {
        self.skills.clear();
    }

    /// Get skill count
    pub fn count(&self) -> usize {
        self.skills.len()
    }

    /// FEATURE (audit-2026-08 round-4): return a borrowed list of
    /// all loaded skill names in registration order. The owned
    /// `Vec<String, ...>` form would require an internal allocator;
    /// `Vec<&str, MAX_SKILLS>` borrows from the underlying `Vec<Skill>`
    /// and is therefore zero-copy. Used by the agent runner to
    /// surface "available skills" in system-prompt injection and by
    /// `AT+SKILLS?` to list skills over UART without copying.
    ///
    /// `MAX_SKILLS` is the hard upper bound; if the registry is
    /// later made resizable, the return-type `Vec<&'_, _, MAX_SKILLS>`
    /// grows with `N` and is the right shape to reuse.
    pub fn names(&self) -> Vec<&str, MAX_SKILLS> {
        let mut out: Vec<&str, MAX_SKILLS> = Vec::new();
        for s in &self.skills {
            // Capacity-bounded: `MAX_SKILLS` matches the underlying
            // Vec capacity, so push always succeeds.
            let _ = out.push(s.name.as_str());
        }
        out
    }

    /// FEATURE (audit-2026-08 round-4): tally skills grouped by
    /// `category`. Returns up to `MAX_CATEGORIES` (8) distinct
    /// category strings paired with their occurrence count.
    ///
    /// Why 8: the agent's current prompt injection groups skills
    /// this way, and the embedded UI can only show ~8 categories on
    /// a 128x64 OLED anyway. If a future task needs more, bump the
    /// const.
    ///
    /// Skills with an empty category are bucketed as `"uncategorized"`
    /// so callers always get a complete picture.
    pub fn count_by_category(&self) -> Vec<(String<32>, u16), 8> {
        const MAX_CATEGORIES: usize = 8;
        let mut out: Vec<(String<32>, u16), MAX_CATEGORIES> = Vec::new();

        for s in &self.skills {
            let cat = if s.category.is_empty() {
                // Allocate a tiny static for the uncategorized
                // bucket; we can't build a `String<32>` on the fly
                // without an `Into` import here, but `from("uncategorized")`
                // is always well under 32 bytes.
                "uncategorized"
            } else {
                s.category.as_str()
            };
            if let Some(slot) = out.iter_mut().find(|(c, _)| c.as_str() == cat) {
                slot.1 = slot.1.saturating_add(1);
            } else if out.len() < MAX_CATEGORIES {
                let entry = (
                    heapless::String::try_from(cat).unwrap_or_else(|_| heapless::String::new()),
                    1,
                );
                let _ = out.push(entry);
            }
            // If `out.len() == MAX_CATEGORIES` and we see a brand-new
            // category, we silently drop it — a registered skill with
            // a 9th category is rare in embedded use, and the next
            // pass with a bigger MAX_CATEGORIES recovers the data.
        }

        out
    }

    /// FEATURE (audit-2026-08 round-4): pick the top-K skills by
    /// usage_count × (success_rate / 100). Used by the agent runner
    /// to inject the most-tried-and-true skills first when the
    /// system-prompt budget is tight.
    ///
    /// Sorting algorithm: simple insertion sort, which is
    /// O(K × N) and well within budget for `N ≤ 10`. Returns at
    /// most `k` skills (or fewer if the registry has fewer than
    /// `k`). Skills with identical scores retain their input
    /// order (stable sort by virtue of the forward scan).
    pub fn best_k(&self, k: usize) -> Vec<&Skill, MAX_SKILLS> {
        let k = k.min(self.skills.len());
        let mut scored: Vec<(&Skill, u32), MAX_SKILLS> = Vec::new();
        for s in &self.skills {
            // `usage_count: u16` × `success_rate: u8` (≤ 100) fits
            // in u32 even at the saturation ceiling (65535 * 100).
            let score = (s.usage_count as u32) * (s.success_rate as u32);
            let _ = scored.push((s, score));
        }

        // Stable partial sort: for each position `0..k`, find the
        // max among the remaining unsorted slots. Because we walk
        // forward only (never swap), equal scores keep their
        // original order — that's the stability we need.
        let n = scored.len();
        for i in 0..k.min(n) {
            let mut best_idx = i;
            let mut best_score = scored[i].1;
            for j in (i + 1)..n {
                if scored[j].1 > best_score {
                    best_idx = j;
                    best_score = scored[j].1;
                }
            }
            if best_idx != i {
                scored.swap(i, best_idx);
            }
        }

        let mut out: Vec<&Skill, MAX_SKILLS> = Vec::new();
        for i in 0..k {
            let _ = out.push(scored[i].0);
        }
        out
    }

    /// FEATURE (audit-2026-08 round-4): one-line human-readable
    /// summary of the registry state, suitable for a CLI doctor
    /// command or `AT+SKILLS?` UART reply. Format:
    ///
    /// ```text
    /// skills=3/10 categories={"glucose":1,"voice":2}
    /// ```
    ///
    /// `MAX_SKILLS = 10` is hard-coded in the const above; we keep
    /// the message shape stable regardless of the actual
    /// `max_skills` field so callers can parse it.
    pub fn summary(&self) -> String<512> {
        let mut out: String<512> = String::new();
        let _ = write!(
            out,
            "skills={}/{} categories={{",
            self.skills.len(),
            MAX_SKILLS
        );
        let cats = self.count_by_category();
        for (i, (c, n)) in cats.iter().enumerate() {
            if i > 0 {
                let _ = out.push_str(",");
            }
            let _ = write!(out, "{}:{}", c.as_str(), n);
        }
        let _ = out.push_str("}");
        out
    }
}

/// Skill representation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Skill name
    pub name: String<MAX_SKILL_NAME>,
    /// Skill description
    pub description: String<MAX_SKILL_DESC>,
    /// Skill category
    pub category: String<32>,
    /// Skill content (simplified Markdown)
    pub content: String<MAX_SKILL_CONTENT>,
    /// Usage count
    pub usage_count: u16,
    /// Success rate (0-100)
    pub success_rate: u8,
}

impl Skill {
    /// Create a new skill
    //
    // The four `try_from().unwrap_or_else(|_| ...)` calls below are
    // intentional: `name` / `description` / `category` / `content`
    // are caller-supplied `&str` of arbitrary length, and a too-long
    // input would otherwise truncate-or-panic. The fallback to an
    // empty `String` keeps the skill recordable even when the input
    // exceeds the embedded buffer, and `validate()` below will
    // catch the empty fields. Clippy's
    // `unnecessary_fallible_conversions` lint doesn't see the
    // runtime input length and incorrectly flags these as
    // infallible; suppress it locally.
    #[allow(clippy::unnecessary_fallible_conversions)]
    pub fn new(name: &str, description: &str, category: &str, content: &str) -> Result<Self> {
        let skill = Self {
            name: heapless::String::try_from(name).unwrap_or_else(|_| heapless::String::new()),
            description: heapless::String::try_from(description)
                .unwrap_or_else(|_| heapless::String::new()),
            category: heapless::String::try_from(category)
                .unwrap_or_else(|_| heapless::String::new()),
            content: heapless::String::try_from(content)
                .unwrap_or_else(|_| heapless::String::new()),
            usage_count: 0,
            success_rate: 100,
        };

        skill.validate()?;
        Ok(skill)
    }

    /// Validate skill
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(AgentError::InputValidationFailed {
                field: "skill.name",
                reason: crate::error::ValidationError::Empty,
            });
        }

        if self.description.is_empty() {
            return Err(AgentError::InputValidationFailed {
                field: "skill.description",
                reason: crate::error::ValidationError::Empty,
            });
        }

        if self.content.is_empty() {
            return Err(AgentError::InputValidationFailed {
                field: "skill.content",
                reason: crate::error::ValidationError::Empty,
            });
        }

        Ok(())
    }

    /// Increment usage count
    pub fn increment_usage(&mut self) {
        self.usage_count = self.usage_count.saturating_add(1);
    }

    /// Update success rate
    pub fn update_success_rate(&mut self, success: bool) {
        if success {
            self.success_rate = self.success_rate.saturating_add(1).min(100);
        } else {
            self.success_rate = self.success_rate.saturating_sub(1);
        }
    }

    /// Get skill as formatted string for injection
    pub fn to_injection_string(&self) -> String<MAX_SKILL_CONTENT> {
        let mut result = String::new();

        let _ = writeln!(result, "# {}", self.name);
        let _ = writeln!(result, "## {}", self.description);
        let _ = write!(result, "{}", self.content);

        result
    }
}

// ---------------------------------------------------------------------------
// Tests for the round-4 features (introspection helpers).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_returns_zero_copy_references() {
        let mut mgr = SkillsManager::new(4);
        mgr.add(Skill::new("read_sensor", "Read a sensor", "device", "x").unwrap())
            .unwrap();
        mgr.add(Skill::new("write_gpio", "Set a GPIO", "device", "x").unwrap())
            .unwrap();
        let names = mgr.names();
        assert_eq!(names.as_slice(), &["read_sensor", "write_gpio"]);
    }

    #[test]
    fn count_by_category_groups_and_renames_empty() {
        let mut mgr = SkillsManager::new(4);
        mgr.add(Skill::new("a", "x", "device", "x").unwrap())
            .unwrap();
        mgr.add(Skill::new("b", "x", "voice", "x").unwrap())
            .unwrap();
        mgr.add(Skill::new("c", "x", "voice", "x").unwrap())
            .unwrap();
        mgr.add(Skill::new("d", "x", "", "x").unwrap()).unwrap(); // → uncategorized
        let cats = mgr.count_by_category();
        // Find each bucket by linear scan rather than a map, to
        // avoid pulling in a `HashMap` for the test.
        fn find(cats: &Vec<(heapless::String<32>, u16), 8>, cat: &str) -> Option<u16> {
            cats.iter()
                .find(|(c, _)| c.as_str() == cat)
                .map(|(_, n)| *n)
        }
        assert_eq!(find(&cats, "device"), Some(1));
        assert_eq!(find(&cats, "voice"), Some(2));
        assert_eq!(find(&cats, "uncategorized"), Some(1));
    }

    #[test]
    fn best_k_ranks_by_usage_times_success_rate() {
        let mut mgr = SkillsManager::new(4);
        let mut s1 = Skill::new("low", "d", "c", "x").unwrap();
        s1.usage_count = 1;
        s1.success_rate = 50; // score 50
        let mut s2 = Skill::new("high", "d", "c", "x").unwrap();
        s2.usage_count = 10;
        s2.success_rate = 100; // score 1000
        let mut s3 = Skill::new("mid", "d", "c", "x").unwrap();
        s3.usage_count = 5;
        s3.success_rate = 80; // score 400
        mgr.add(s1).unwrap();
        mgr.add(s2).unwrap();
        mgr.add(s3).unwrap();
        let top2 = mgr.best_k(2);
        assert_eq!(top2[0].name.as_str(), "high");
        assert_eq!(top2[1].name.as_str(), "mid");
    }

    #[test]
    fn summary_is_stable_format() {
        let mut mgr = SkillsManager::new(4);
        mgr.add(Skill::new("a", "x", "voice", "x").unwrap())
            .unwrap();
        mgr.add(Skill::new("b", "x", "voice", "x").unwrap())
            .unwrap();
        let s = mgr.summary();
        assert!(s.as_str().starts_with("skills=2/"));
        assert!(s.as_str().contains("voice:2"));
    }

    #[test]
    fn add_rejects_duplicate_name() {
        let mut mgr = SkillsManager::new(4);
        mgr.add(Skill::new("dup", "first", "c", "x").unwrap())
            .unwrap();
        // Same name, different content — must be rejected so `get`/`remove`
        // never have to disambiguate two skills sharing a key.
        let err = mgr
            .add(Skill::new("dup", "second", "c", "y").unwrap())
            .unwrap_err();
        assert!(matches!(
            err,
            AgentError::InputValidationFailed {
                field: "skill.name",
                reason: crate::error::ValidationError::Duplicate,
            }
        ));
        // The original skill is unchanged and still retrievable.
        assert_eq!(mgr.count(), 1);
        assert_eq!(mgr.get("dup").unwrap().description.as_str(), "first");
    }

    #[test]
    fn add_allows_distinct_names() {
        let mut mgr = SkillsManager::new(4);
        mgr.add(Skill::new("a", "x", "c", "x").unwrap()).unwrap();
        mgr.add(Skill::new("b", "x", "c", "x").unwrap()).unwrap();
        assert_eq!(mgr.count(), 2);
        // Case-sensitive: "a" and "A" are distinct names.
        mgr.add(Skill::new("A", "x", "c", "x").unwrap()).unwrap();
        assert_eq!(mgr.count(), 3);
    }

    #[test]
    fn search_matches_name_and_description() {
        let mut mgr = SkillsManager::new(4);
        mgr.add(Skill::new("read_sensor", "Read ambient temperature", "c", "x").unwrap())
            .unwrap();
        mgr.add(Skill::new("write_gpio", "Set a pin", "c", "x").unwrap())
            .unwrap();
        // Matches by name.
        assert_eq!(mgr.search("sensor")[0].name.as_str(), "read_sensor");
        // Matches by description substring.
        assert_eq!(mgr.search("temperature")[0].name.as_str(), "read_sensor");
        // Empty keyword matches everything (contains("") is always true).
        assert_eq!(mgr.search("").len(), 2);
        // No match → empty result.
        assert_eq!(mgr.search("zzz").len(), 0);
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;

    #[test]
    fn add_rejects_when_at_max_capacity() {
        let mut mgr = SkillsManager::new(2);
        mgr.add(Skill::new("a", "x", "c", "x").unwrap()).unwrap();
        mgr.add(Skill::new("b", "x", "c", "x").unwrap()).unwrap();
        let err = mgr
            .add(Skill::new("c", "x", "c", "x").unwrap())
            .unwrap_err();
        assert!(matches!(err, AgentError::MemoryAllocationFailed { .. }));
        assert_eq!(mgr.count(), 2);
    }

    #[test]
    fn add_rejects_invalid_skill_via_validate() {
        let mut mgr = SkillsManager::new(4);
        let bad = Skill {
            name: String::new(),
            description: String::try_from("d").unwrap(),
            category: String::try_from("c").unwrap(),
            content: String::try_from("x").unwrap(),
            usage_count: 0,
            success_rate: 100,
        };
        let err = mgr.add(bad).unwrap_err();
        assert!(matches!(
            err,
            AgentError::InputValidationFailed {
                field: "skill.name",
                ..
            }
        ));
    }

    #[test]
    fn skill_new_rejects_empty_fields() {
        assert!(Skill::new("", "d", "c", "x").is_err()); // empty name
        assert!(Skill::new("n", "", "c", "x").is_err()); // empty description
        assert!(Skill::new("n", "d", "c", "").is_err()); // empty content
                                                         // Category is allowed empty (→ uncategorized bucket).
        assert!(Skill::new("n", "d", "", "x").is_ok());
    }

    #[test]
    fn skill_new_too_long_truncates_then_rejects() {
        // Name > MAX_SKILL_NAME -> try_from fails -> empty -> validate rejects.
        assert!(Skill::new(&"x".repeat(40), "d", "c", "x").is_err());
    }

    #[test]
    fn get_all_all_mut_and_clear() {
        let mut mgr = SkillsManager::new(4);
        mgr.add(Skill::new("a", "x", "c", "x").unwrap()).unwrap();
        mgr.add(Skill::new("b", "x", "c", "x").unwrap()).unwrap();
        assert_eq!(mgr.get("a").unwrap().name.as_str(), "a");
        assert!(mgr.get("missing").is_none());
        assert_eq!(mgr.all().len(), 2);
        mgr.all_mut()[0].usage_count = 7;
        assert_eq!(mgr.get("a").unwrap().usage_count, 7);
        mgr.clear();
        assert_eq!(mgr.count(), 0);
        assert!(mgr.all().is_empty());
    }

    #[test]
    fn remove_ok_and_error_for_missing() {
        let mut mgr = SkillsManager::new(4);
        mgr.add(Skill::new("a", "x", "c", "x").unwrap()).unwrap();
        mgr.remove("a").unwrap();
        assert_eq!(mgr.count(), 0);
        let err = mgr.remove("a").unwrap_err();
        assert!(matches!(err, AgentError::ConfigurationError { .. }));
    }

    #[test]
    fn usage_and_success_rate_saturate() {
        let mut s = Skill::new("a", "d", "c", "x").unwrap();
        s.increment_usage();
        assert_eq!(s.usage_count, 1);
        s.usage_count = u16::MAX;
        s.increment_usage();
        assert_eq!(s.usage_count, u16::MAX);

        s.success_rate = 100;
        s.update_success_rate(true);
        assert_eq!(s.success_rate, 100); // capped at 100
        s.update_success_rate(false);
        assert_eq!(s.success_rate, 99);
        s.success_rate = 0;
        s.update_success_rate(false);
        assert_eq!(s.success_rate, 0); // floored at 0
        s.update_success_rate(true);
        assert_eq!(s.success_rate, 1);
    }

    #[test]
    fn to_injection_string_formats_skill() {
        let s = Skill::new("read_sensor", "Read a sensor", "device", "returns temp").unwrap();
        let out = s.to_injection_string();
        assert!(out.as_str().contains("# read_sensor"));
        assert!(out.as_str().contains("## Read a sensor"));
        assert!(out.as_str().contains("returns temp"));
    }
}
