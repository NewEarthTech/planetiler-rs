# Entry point for the estate's justfiles: each infra island is a module, its
# recipes run in its own directory. `just --list` shows the tree;
# e.g. `just runner::deploy`, `just docs::diff`, `just ci::deploy`.

mod ci "infra/ci"
mod docs "infra/docs"
mod runner "infra/runner"
