# Agent instructions

## Tests

A test written after the code, with expected values read off that code, repeats the implementation: it passes by construction, misses the bugs it shares with the code, and breaks on every refactor. Do not write such tests unless the user asks for a specific one.

- Verify features end to end. Run the real entry point on real or fixed input and leave an artifact another person can rerun and compare, such as an output file, log, report, or screenshot. Give the command and the artifact path in the final message.
- When a unit needs an isolated test, first list the ways it can fail, take expected values from the spec or a hand calculation, and only then write the code.
- A bug fix may add one test that reproduces the bug and fails before the fix.
- Keep or add a test only if losing it would let a security, money, data-loss, or reported-number bug ship unnoticed and no end-to-end run covers it.
- If a refactor that keeps behavior breaks a test, the test was checking implementation. Delete it instead of rewriting it and list it in the PR.
- Do not test constants, prompt or message strings, output formatting, internal helpers, or fakes built for the test itself.

End-to-end path here: the built binary with temporary `CLAUDE_ACCOUNT_KIT_HOME` and `CLAUDE_CONFIG_DIR`; the artifact is the resulting config and credential files. Touch the real Keychain item only when the user asks. Tests that meet the keep bar: credential writes, where a bug loses a login, and the routing decision. A truncated Keychain write is caught at run time, not by a test: `upsert` in `src/credentials/keychain.rs` reads each write back, compares it, and restores the previous value on a mismatch. Keep that read-back, and keep passing the value to `security -w` as an argument; its stdin prompt keeps only 128 bytes of a credential that is over two kilobytes.
