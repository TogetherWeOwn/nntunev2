# Database migrations

Every SQL migration is recorded once in `manifest.toml`, in strictly increasing numeric order. Use the next four-digit number after the current highest number for each new file. The gate requires unique, increasing numbers but does not require existing migration sequences to be gap-free.

Add a manifest entry for every new migration with its explicit `target` (`app` or `mirror`) and the lowercase SHA-256 checksum of the file contents. The CI migration gate validates the manifest, numbering, targets, and checksums.

Never edit or rename an applied migration. Migrations are forward-only: correct a deployed schema with a new migration, not by changing history.
