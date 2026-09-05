# Security policy

Please report security issues privately through GitHub's security advisory feature instead of a public issue.

The production example deliberately restricts the display container's SSH key to
the host-side `aster-pve-snapshot` command. Do not place private keys, passwords,
API tokens, or environment-specific configuration in the repository.

This project controls reverse-engineered hardware interfaces. Review the warning
in the main README before using it on a system you cannot easily recover.
