# Default recipe to show available commands
default:
    @just --list

# Open the development database
db-connect:
    sqlite3 lnaddrd.sqlite3

# Explicitly initialize a development service
init relay:
    cargo run -- --domains localhost --root-secret-file root-secret --admin-password-file admin-password --nostr-relays {{relay}} initialize-empty

# Run the development server with the local SQLite database
run relay:
    cargo run -- --domains localhost --root-secret-file root-secret --admin-password-file admin-password --nostr-relays {{relay}}

# Format the code
format:
    cargo fmt --all

clippy:
    cargo clippy --all --all-targets -- -D warnings

test:
    cargo test --all

# Serve the static marketplace on http://localhost:8081
marketplace-serve:
    python3 -m http.server 8081 -d marketplace
