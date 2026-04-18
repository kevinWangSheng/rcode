## ADDED Requirements

### Requirement: In-File Cancel for Grep

The Grep tool SHALL check its cancellation token periodically (at
minimum every 512 lines) during a single file's line iteration. It
MUST NOT rely solely on a cancel check between files.

On cancellation the tool SHALL close the file and return a
`CcError::tool("Grep cancelled")` result promptly.

#### Scenario: Cancel during a 10 GB file
- **GIVEN** Grep running against a single 10 GB file
- **WHEN** the cancellation token fires during iteration
- **THEN** the tool returns an error within a bounded number of lines
  (at most one check window worth)
- **AND** does not read the file to EOF
