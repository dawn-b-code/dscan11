# Agent Commit Metadata

When an agent creates a git commit in this repository, the commit message must
include a tag in this exact format:

```text
[<agent> was here]
```

Replace `<agent>` with the agent name or identifier used for the work.

Add model information as an additional commit message comment or trailer so the
commit can be traced later. Include at least the model name, and include version
or reasoning-effort details when available.

Example:

```text
Tighten scan cache limits

[codex was here]

Model: GPT-5
Reasoning-Effort: medium
```

## Build Housekeeping

When a build is completed and tests have passed, update the necessary project
documentation and the `dscan11 --help` output so they match the current CLI
behavior before finishing the work.

Do the final compile only after those documentation and help updates are in
place. This keeps the release check anchored to the exact user-facing behavior
that will be shipped.

At the end of any commit flow, rebuild the Windows GNU release binary so the
shipped executable is fresh:

```powershell
cargo build --release --target x86_64-pc-windows-gnu
```

Verify the updated binary under:

```text
.\target\x86_64-pc-windows-gnu\release\
```
