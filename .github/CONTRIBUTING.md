# Contributing to ValiraVPN for Desktop

## Before you start

Open an issue before writing code for anything larger than a fix. It is easier to agree
on an approach in a paragraph than in a pull request.

For security problems, do not open an issue. See [SECURITY.md](SECURITY.md).

## Licence and contributions

This project is released under the PolyForm Noncommercial 1.0.0 licence, which reserves
commercial use to ValiraVPN. By submitting a contribution you agree that ValiraVPN may
license your contribution on those same terms, including commercially.

If you are not willing to do that, please do not submit a pull request. It is better to
say so in an issue than to have work turned down after it is written.

## Pull requests

* One subject per pull request.
* `cargo build --release` and `cargo test --lib` must pass on Windows and Linux. CI runs
  both.
* Commit messages state what changed and why. The why is the part that is not already in
  the diff.
* Explain how you tested it. "It builds" is not a test.

## Code

Match the surrounding code rather than a style guide. Comments explain decisions, not
mechanics: what the code does is visible, why it does it that way is not.
