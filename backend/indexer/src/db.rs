use sqlx::{postgres::PgPoolOptions, PgPool};

pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}

pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS trades (
            id SERIAL PRIMARY KEY,
            signature TEXT NOT NULL,
            owner TEXT NOT NULL,
            side BOOLEAN NOT NULL,
            size BIGINT NOT NULL,
            price BIGINT NOT NULL,
            timestamp TIMESTAMPTZ DEFAULT NOW()
        );

        CREATE TABLE IF NOT EXISTS positions (
            id SERIAL PRIMARY KEY,
            address TEXT UNIQUE NOT NULL,
            owner TEXT NOT NULL,
            market TEXT NOT NULL,
            side BOOLEAN NOT NULL,
            size BIGINT NOT NULL,
            collateral BIGINT NOT NULL,
            entry_price BIGINT NOT NULL,
            leverage INTEGER NOT NULL,
            status TEXT NOT NULL,
            opened_at TIMESTAMPTZ NOT NULL,
            updated_at TIMESTAMPTZ DEFAULT NOW()
        );

        CREATE TABLE IF NOT EXISTS orders (
            id SERIAL PRIMARY KEY,
            address TEXT UNIQUE NOT NULL,
            owner TEXT NOT NULL,
            side TEXT NOT NULL,
            order_type TEXT NOT NULL,
            price BIGINT NOT NULL,
            size BIGINT NOT NULL,
            filled_size BIGINT NOT NULL,
            status TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL
        );

        CREATE TABLE IF NOT EXISTS funding_snapshots (
            id SERIAL PRIMARY KEY,
            rate BIGINT NOT NULL,
            long_oi BIGINT NOT NULL,
            short_oi BIGINT NOT NULL,
            price BIGINT NOT NULL,
            timestamp TIMESTAMPTZ DEFAULT NOW()
        );

        CREATE INDEX IF NOT EXISTS idx_trades_owner ON trades(owner);
        CREATE INDEX IF NOT EXISTS idx_trades_timestamp ON trades(timestamp);
        CREATE INDEX IF NOT EXISTS idx_positions_owner ON positions(owner);
        CREATE INDEX IF NOT EXISTS idx_orders_owner ON orders(owner);
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_trade(
    pool: &PgPool,
    signature: &str,
    owner: &str,
    side: bool,
    size: u64,
    price: u64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO trades (signature, owner, side, size, price) VALUES ($1, $2, $3, $4, $5)"
    )
    .bind(signature)
    .bind(owner)
    .bind(side)
    .bind(size as i64)
    .bind(price as i64)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_position(
    pool: &PgPool,
    address: &str,
    owner: &str,
    market: &str,
    side: bool,
    size: u64,
    collateral: u64,
    entry_price: u64,
    leverage: u32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO positions (address, owner, market, side, size, collateral, entry_price, leverage, status, opened_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'open', NOW())
        ON CONFLICT (address) DO UPDATE SET
            size = EXCLUDED.size,
            collateral = EXCLUDED.collateral,
            updated_at = NOW()
        "#
    )
    .bind(address)
    .bind(owner)
    .bind(market)
    .bind(side)
    .bind(size as i64)
    .bind(collateral as i64)
    .bind(entry_price as i64)
    .bind(leverage as i32)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_funding_snapshot(
    pool: &PgPool,
    rate: i64,
    long_oi: u64,
    short_oi: u64,
    price: u64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO funding_snapshots (rate, long_oi, short_oi, price) VALUES ($1, $2, $3, $4)"
    )
    .bind(rate)
    .bind(long_oi as i64)
    .bind(short_oi as i64)
    .bind(price as i64)
    .execute(pool)
    .await?;

    Ok(())
}
