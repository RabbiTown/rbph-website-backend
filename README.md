# rbph-website-backend

**Project RBPH - Backend**

This project is in the very early stages of development. Version migrations are **NOT GUARANTEED**.

## Development

1. Prepare PostgreSQL and Redis services.
2. Install the Rust toolchain and `sqlx-cli`.
3. Copy `.env.example` to `.env` and change the required configuration values.
4. Edit `config.toml` and adjust the necessary options.  
   Ensure `db_addr` matches `DATABASE_URL` in `.env`.
5. `sqlx migrate run` - Run database migrations.
6. `cargo run` - Start the application.

Before committing, make sure to run:

- `cargo sqlx prepare`
- `cargo clippy --fix --allow-dirty`
- `cargo fmt --all`
