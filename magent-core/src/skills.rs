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

        // Validate skill
        skill.validate()?;

        self.skills.push(skill).map_err(|_| AgentError::MemoryAllocationFailed {
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
        let pos = self
            .skills
            .iter()
            .position(|s| s.name == name)
            .ok_or(AgentError::ConfigurationError {
                field: "skill",
                reason: crate::error::ConfigError::MissingField,
            })?;

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
    pub fn new(
        name: &str,
        description: &str,
        category: &str,
        content: &str,
    ) -> Result<Self> {
        let skill = Self {
            name: heapless::String::try_from(name).unwrap_or_else(|_| heapless::String::new()),
            description: heapless::String::try_from(description).unwrap_or_else(|_| heapless::String::new()),
            category: heapless::String::try_from(category).unwrap_or_else(|_| heapless::String::new()),
            content: heapless::String::try_from(content).unwrap_or_else(|_| heapless::String::new()),
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

