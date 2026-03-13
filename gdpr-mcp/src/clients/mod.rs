// HTTP / external service clients.
//
// ClickHouse audit streaming is a future extension — documents and audit records
// are written to rusqlite for now, which is sufficient for single-node deployments.
// When ClickHouse becomes a hard requirement, add a `clickhouse::Client` here and
// wire it into `AppState`.
