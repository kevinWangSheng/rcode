## ADDED Requirements

### Requirement: Type-Preserving Settings Merge

`Settings::merge` SHALL reject mergers where the same key has
incompatible shapes (object vs array vs scalar) across layers. It MUST
return a `CcError::Config` whose message names the field path and the
two layer identifiers.

Fields listed in the known-collection whitelist (`hooks`,
`permissions`, `additional_contexts`) SHALL continue to array-concat
when both layers provide arrays.

Scalar-to-scalar replacements of the same JSON type SHALL proceed as
today.

#### Scenario: Type mismatch errors
- **GIVEN** user settings with `{"a": {"x": 1}}`
- **AND** project settings with `{"a": [1, 2]}`
- **WHEN** settings are loaded
- **THEN** load returns `Err(CcError::Config { .. })` naming field
  `a` and both file paths

#### Scenario: Hooks concat across layers
- **GIVEN** user settings with `{"hooks": [h1]}`
- **AND** project settings with `{"hooks": [h2]}`
- **WHEN** settings are loaded
- **THEN** merged settings contain `[h1, h2]`

#### Scenario: Scalar replace within same type
- **GIVEN** user settings `{"model": "opus-4-6"}` and project
  `{"model": "haiku-4-5"}`
- **WHEN** settings are loaded
- **THEN** merged `model` is `"haiku-4-5"` (higher-priority layer wins)
