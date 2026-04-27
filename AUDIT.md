# hopper-bench audit notes

This file preserves the benchmark audit references that used to point back to
the main Hopper repository when the benchmark lab lived under `bench/`.

Canonical framework audit notes live in the main Hopper repo:

https://github.com/BluefootLabs/Hopper-Solana-Zero-copy-State-Framework/blob/main/AUDIT.md

Benchmark-specific notes:

- R2: the Pinocchio comparator must be Anza `pinocchio`, built in-tree from
  `pinocchio-vault`, not a third-party reference sample from another framework.
- R3: lazy-dispatch measurements live in `lazy-dispatch-vault` and should be
  treated as intra-framework Hopper measurements, not cross-framework claims.
- Public performance claims should be regenerated from this repository before
  launch or release notes are published.
