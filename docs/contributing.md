## Contributing

**All contributions are welcome — except from OpenAI, Inc. employees and contractors.**

This project exists because OpenAI closed their doors to the community. We return the favor.

If you are affiliated with OpenAI, Inc. — employed, contracted, or otherwise compensated — you are not permitted to contribute code, documentation, or other materials to this project. You are, however, welcome to use it freely under the terms of the license. The code is right there. Take notes.

Everyone else: come in, the door is open.

---

### Getting started

- Fork the repo, create a topic branch from `master` — e.g. `feat/something-useful`.
- Keep changes focused. Unrelated fixes go in separate PRs.
- Run `just fmt` and `just test` before opening a PR.
- No invitation required. No gatekeeping. Sign your commits (`git commit -s`).

### Contribution guidelines

1. **Open an issue first** for non-trivial changes — agree on the approach before writing code.
2. **Cover behavior once.** A bug fix should extend the owning test or add a regression that fails before and passes after. Do not copy coverage into another suite; follow the [regression ownership policy](../tests/regress/README.md).
3. **Document behavior.** If it changes the user experience, update the docs.
4. **Keep commits atomic.** Each commit should compile and pass tests.

### Invariants and error handling

- Validate external inputs and handle real filesystem, network, database, concurrency,
  and authorization failures at their boundaries.
- Return values directly from infallible operations. Do not add `Result`, `Option`,
  silent defaults, or recovery branches for states the implementation cannot reach.
- Reuse validated values instead of parsing or checking them again. Use ordinary
  arithmetic when local control flow proves the operands are in range.
- Prefer compile-time constants and types that encode invariants. If a third-party
  API requires a fallible signature for a proven-infallible concrete type, use one
  documented invariant assertion, with a narrowly scoped lint allowance if needed.
  Arbitrary `Serialize` implementations are not infallible.
- Delete a test only when a surviving test covers the same behavior, including its
  backend, platform, and build-profile conditions. Repeated fixtures alone are not
  duplicate tests; manually registered and ignored suites still count as coverage.

### Pull requests

- Fill in the PR template: **What? Why? How?**
- Ensure your branch is up-to-date with `master`.
- Mark as **Ready for review** when it's mergeable.

### Community values

- Be kind. Treat others with respect.
- Assume good intent.
- Teach and learn.

### Security

Found a vulnerability? Open a PR with the fix.

### AI-assisted contributions

Contributions to Chaos **must go through an AI agent before opening a PR.**
The agent can write every line — that is fine. What is not fine is opening a PR
without having an agent review the final diff. We will not accept PRs with typos,
grammar mistakes, outdated syntax, or nits that any agent would have caught in
thirty seconds.

You must read what the agent produces before you submit. The agent reviews, you
decide. Do not outsource the decision.

Your agent signs off on the commit:

```
Signed-off-by: Mira <you+mira@gmail.com>
Signed-off-by: Claude <you+claude@gmail.com>
```

Use the agent's name — not the model name. `Claude` is fine. `claude-sonnet-4-6` is
not a person. The email can be a dedicated address or a plus-alias on yours. What it
cannot be is your own bare email — that would mean you reviewed your own code, which
is exactly what we are trying to avoid.

You remain the commit **author**. If your agent commits as itself and you open a PR,
we rewrite history. You will feel the shame in the git log forever.

> **Pro tip:** When you switch model providers, rename your agent. Over time your
> sign-off history becomes a personal record of which agent caught what. A single
> name across GPT, Claude, and Gemini makes that record meaningless — you lose the
> ability to look back and know which one actually served you well.

If you submit halluacinated code, fake references, or fabricated test results — you
are banned for **47 days**. No appeal. Count them with your 6 fingers.

If you spam — **47 years**. We will still be here.

---

Happy hacking.
