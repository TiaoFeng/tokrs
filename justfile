default:
    @just --list

ppy:
    cargo clippy --all-targets --all-features -- -D warnings

fmt:
    cargo fmt --all -- --check

test:
    cargo test --all-features

cover:
    cargo llvm-cov

check: fmt ppy test cover
    @echo "All checks passed"