# Contributing to mAgent

Thank you for your interest in contributing to mAgent! This document provides guidelines for contributing to the project.

## Code of Conduct

- Be respectful and inclusive
- Provide constructive feedback
- Focus on what is best for the community
- Show empathy towards other community members

## Development Setup

### Prerequisites

- Rust 1.70 or higher
- arm-none-eabi toolchain
- nRF52840 Development Kit (optional)
- probe-rs for flashing (optional)

### Installation

```bash
# Install Rust target
rustup target add thumbv7em-none-eabihf

# Install cargo-binutils
cargo install cargo-binutils

# Install probe-rs (for hardware flashing)
cargo install probe-rs

# Clone repository
git clone https://github.com/arksong/magent.git
cd magent

# Build
cargo build --release
```

## Coding Standards

### Rust Guidelines

- Follow [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Use `cargo clippy` for linting
- Use `cargo fmt` for formatting
- Document all public APIs
- Write unit tests for all modules

### Aerospace-Grade Standards

- **No Panics**: All functions must return `Result<T>`
- **Memory Safety**: Use heapless data structures
- **Input Validation**: Validate all external inputs
- **Error Handling**: Use comprehensive error types
- **Resource Limits**: Enforce memory and iteration budgets
- **Documentation**: Document safety-critical code

### Code Style

```rust
// Good: Returns Result
pub fn safe_function(input: &str) -> Result<String> {
    if input.len() > MAX_LENGTH {
        return Err(AgentError::InputValidationFailed { ... });
    }
    Ok(String::from(input))
}

// Bad: Uses unwrap
pub fn unsafe_function(input: &str) -> String {
    String::from(input).unwrap()  // DON'T DO THIS
}
```

## Testing

### Unit Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_budget_enforcer

# Run with output
cargo test -- --nocapture
```

### Integration Tests

```bash
# Run integration tests (requires hardware)
cargo test --features integration
```

### Memory Analysis

```bash
# Check binary size
cargo size --release

# Analyze stack usage
cargo stack-sizes
```

## Pull Request Process

### Before Submitting

1. Update documentation
2. Add/update tests
3. Run `cargo clippy`
4. Run `cargo fmt`
5. Ensure all tests pass
6. Update CHANGELOG.md

### Pull Request Template

```markdown
## Description
Brief description of changes

## Type of Change
- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Documentation update

## Testing
- [ ] Unit tests added/updated
- [ ] Integration tests added/updated
- [ ] Manual testing performed

## Checklist
- [ ] Code follows project style guidelines
- [ ] Self-review of code completed
- [ ] Comments added for complex logic
- [ ] Documentation updated
- [ ] No new warnings generated
- [ ] Tests added/updated
- [ ] All tests passing
```

## Project Structure

```
magent/
├── magent-core/          # Core library
│   ├── src/
│   │   ├── agent.rs     # ReAct state machine
│   │   ├── error.rs     # Error handling
│   │   ├── safety.rs    # Safety mechanisms
│   │   ├── skills.rs    # Skills system
│   │   ├── tools.rs     # Tool registry
│   │   ├── storage.rs   # Flash storage
│   │   ├── communication.rs  # BLE/Thread
│   │   └── config.rs    # Configuration
│   └── Cargo.toml
├── magent-app/          # Application
│   ├── src/
│   │   └── main.rs      # Entry point
│   └── Cargo.toml
├── docs/                # Documentation
│   ├── ARCHITECTURE.md
│   ├── API.md
│   ├── HARDWARE.md
│   └── SAFETY.md
├── tests/               # Integration tests
├── Cargo.toml           # Workspace config
├── memory.x             # Linker script
└── README.md
```

## Adding New Features

### 1. Design Phase

- Create an issue describing the feature
- Discuss implementation approach
- Consider memory and performance impact
- Plan testing strategy

### 2. Implementation Phase

- Add feature to appropriate module
- Follow aerospace-grade standards
- Add comprehensive error handling
- Write unit tests

### 3. Documentation Phase

- Update API documentation
- Add usage examples
- Update architecture docs
- Add to CHANGELOG.md

### 4. Review Phase

- Self-review code
- Request peer review
- Address feedback
- Update tests/docs

## Bug Reports

### Bug Report Template

```markdown
## Description
Clear description of the bug

## Steps to Reproduce
1. Step 1
2. Step 2
3. Step 3

## Expected Behavior
What should happen

## Actual Behavior
What actually happens

## Environment
- Hardware: nRF52840 DK
- Rust version: 1.70.0
- mAgent version: 0.1.0

## Additional Context
Logs, screenshots, etc.
```

## Feature Requests

### Feature Request Template

```markdown
## Problem Description
What problem does this solve?

## Proposed Solution
How should this be implemented?

## Alternatives Considered
What other approaches were considered?

## Additional Context
Any other relevant information
```

## Documentation

### Writing Documentation

- Use clear, concise language
- Provide code examples
- Include diagrams where helpful
- Keep documentation up to date

### API Documentation

```rust
/// Brief description of what this does
///
/// More detailed explanation...
///
/// # Arguments
///
/// * `arg1` - Description of arg1
/// * `arg2` - Description of arg2
///
/// # Returns
///
/// * `Result<T>` - Description of return value
///
/// # Errors
///
/// * `AgentError::MemoryAllocationFailed` - When...
///
/// # Examples
///
/// ```
/// let result = function(arg1, arg2)?;
/// ```
pub fn function(arg1: Type1, arg2: Type2) -> Result<ReturnType> {
    // Implementation
}
```

## Release Process

### Version Bumping

1. Update version in Cargo.toml
2. Update CHANGELOG.md
3. Create git tag
4. Push to repository
5. Create GitHub release

### Changelog Format

```markdown
## [0.2.0] - 2026-XX-XX

### Added
- New feature 1
- New feature 2

### Changed
- Changed behavior 1
- Changed behavior 2

### Fixed
- Bug fix 1
- Bug fix 2

### Removed
- Deprecated feature 1
```

## Security

### Reporting Security Issues

- Do not create public issues
- Email security contact
- Provide detailed description
- Wait for confirmation before disclosure

### Security Guidelines

- Validate all inputs
- Use secure communication
- Follow aerospace standards
- Regular security audits

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

## Questions?

- Open an issue for questions
- Join discussions in issues
- Contact maintainers directly
