# Implémentation : Billing TensorZero Full ClickHouse

**Option C** : Délégation complète du metering à TensorZero/ClickHouse

## Vue d'Ensemble

### Architecture Cible

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Stripe                                         │
│              (Checkout, Subscriptions, Invoices, Portal)                    │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                          OpenCode                                           │
│  ┌─────────────────┐    ┌──────────────────┐    ┌────────────────────────┐  │
│  │  BillingTable  │    │   UsageTable     │    │  reconcileBillingJob  │  │
│  │  (balance,     │◄───│  (tensorzero_    │───►│  (cron: quotidien)    │  │
│  │   Stripe IDs)  │    │   inference_id)  │    │                        │  │
│  └─────────────────┘    └──────────────────┘    └────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
                                      ▲
                                      │ Query
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                         TensorZero                                          │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │                        ClickHouse                                   │    │
│  │  ModelInference(cost, tags[customer_id], timestamp, inference_id)  │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Principes Fondamentaux

1. **TensorZero = Source of Truth** pour le coût
2. **ClickHouse** stocke toutes les métriques d'inférence
3. **OpenCode** dérive sa facturation depuis ClickHouse
4. **Réconciliation quotidienne** pour synchroniser les balances

---

## 1. Schéma de Base de Données

### 1.1 Modification de `UsageTable`

**Fichier**: `packages/console/core/src/schema/billing.sql.ts`

```typescript
export const UsageTable = mysqlTable(
  "usage",
  {
    ...workspaceColumns,
    ...timestamps,
    model: varchar("model", { length: 255 }).notNull(),
    provider: varchar("provider", { length: 255 }).notNull(),
    inputTokens: int("input_tokens").notNull(),
    outputTokens: int("output_tokens").notNull(),
    reasoningTokens: int("reasoning_tokens"),
    cacheReadTokens: int("cache_read_tokens"),
    cacheWrite5mTokens: int("cache_write_5m_tokens"),
    cacheWrite1hTokens: int("cache_write_1h_tokens"),
    // NOUVEAU: Stocké en dollars (DECIMAL) pour précision avec TensorZero
    costDollars: decimal("cost_dollars", { precision: 20, scale: 10 }),
    // NOUVEAU: ID d'inférence TensorZero pour réconciliation
    tensorzeroInferenceId: varchar("tensorzero_inference_id", { length: 36 }),
    // NOUVEAU: Coût consolidé depuis ClickHouse (pour réconciliation)
    reconciledCostDollars: decimal("reconciled_cost_dollars", { precision: 20, scale: 10 }),
    // NOUVEAU: Flag de réconciliation
    reconciledAt: utc("reconciled_at"),
    // NOUVEAU: Statut de synchronisation
    syncStatus: mysqlEnum("sync_status", ["pending", "synced", "discrepancy"]),
    keyID: ulid("key_id"),
    sessionID: varchar("session_id", { length: 30 }),
    enrichment: json("enrichment").$type<{
      plan: "sub" | "byok" | "lite"
      source: "tensorzero" | "internal"
      customerId?: string // Pour jointure avec TensorZero
    }>(),
  },
  (table) => [
    ...workspaceIndexes(table),
    index("usage_time_created").on(table.workspaceID, table.timeCreated),
    // Index pour réconciliation
    index("usage_tensorzero_id").on(table.tensorzeroInferenceId),
    index("usage_sync_status").on(table.syncStatus),
  ],
)
```

### 1.2 Nouvelle Table: `TensorZeroCustomerMapping`

Pour mapper les workspaces OpenCode aux customers TensorZero.

**Fichier**: `packages/console/core/src/schema/billing.sql.ts`

```typescript
export const TensorZeroCustomerTable = mysqlTable(
  "tensorzero_customer",
  {
    ...workspaceColumns,
    ...timestamps,
    // ID customer TensorZero (depuis les tags)
    tensorzeroCustomerId: varchar("tensorzero_customer_id", { length: 255 }).notNull(),
    // Dernière synchronisation
    lastSyncAt: utc("last_sync_at"),
    // Statut
    enabled: boolean("enabled").notNull().default(true),
  },
  (table) => [...workspaceIndexes(table), uniqueIndex("tz_customer_unique").on(table.tensorzeroCustomerId)],
)
```

### 1.3 Index ClickHouse Requis

Le cluster TensorZero doit avoir ces configurations:

```sql
-- Assurer que les tags sont indexés pour les queries de facturation
ALTER TABLE ModelInference ADD INDEX idx_customer_tag mapKeyExists(tags, 'customer_id') TYPE bloom_filter_granularity(1) GRANULARITY 1;

-- Créer une materialized view pour l'agrégation par customer
CREATE MATERIALIZED VIEW IF NOT EXISTS cost_by_customer
ENGINEER = SummingMergeTree()
ORDER BY (customer_id, date)
AS SELECT
    toDate(timestamp) AS date,
    tags['customer_id'] AS customer_id,
    sum(cost) AS total_cost,
    sum(input_tokens) AS total_input_tokens,
    sum(output_tokens) AS total_output_tokens,
    count() AS inference_count
FROM ModelInference
WHERE tags['customer_id'] IS NOT NULL
GROUP BY customer_id, date;
```

---

## 2. Configuration TensorZero

### 2.1 Configuration du Cost Tracking

**Fichier**: `tensorzero.toml` (côté TensorZero)

```toml
[models.gpt-5.providers.openai]
type = "openai"
model_name = "gpt-5"
cost = [
  { pointer = "/usage/prompt_tokens", cost_per_million = 1.25, required = true },
  { pointer = "/usage/completion_tokens", cost_per_million = 10.00, required = true },
  { pointer = "/usage/prompt_tokens_details/cached_tokens", cost_per_million = -1.125 },
]

[models.claude-sonnet-4-20250514.providers.anthropic]
type = "anthropic"
model_name = "claude-sonnet-4-20250514"
cost = [
  { pointer = "/usage/input_tokens", cost_per_million = 3.00, required = true },
  { pointer = "/usage/output_tokens", cost_per_million = 15.00, required = true },
  { pointer = "/usage/cache_creation_input_tokens", cost_per_million = 3.75 },
]
```

### 2.2 Tagging des Inférences

Les inferences doivent être tagguées avec `customer_id` pour la facturation:

```typescript
// Dans le handler OpenCode lors de l'appel à TensorZero
const response = await tensorzero.inference({
  function_name: req.body.function,
  input: req.body.input,
  tags: {
    customer_id: workspaceId, // ID workspace OpenCode
    environment: "production",
  },
})
```

---

## 3. Modifications du Code OpenCode

### 3.1 Extension du Provider Helper

**Fichier**: `packages/console/app/src/routes/zen/util/provider/provider.ts`

```typescript
export type ProviderHelper = (input: { reqModel: string; providerModel: string }) => {
  format: ZenData.Format
  modifyUrl: (providerApi: string, isStream?: boolean) => string
  modifyHeaders: (headers: Headers, body: Record<string, any>, apiKey: string) => void
  modifyBody: (body: Record<string, any>, workspaceID?: string) => Record<string, any>
  createBinaryStreamDecoder: () => ((chunk: Uint8Array) => Uint8Array | undefined) | undefined
  streamSeparator: string
  createUsageParser: () => {
    parse: (chunk: string) => void
    retrieve: () => any
    buidlCostChunk: (cost: string) => string
  }
  normalizeUsage: (usage: any) => UsageInfo
  // NOUVEAU: Extraire le coût depuis la réponse TensorZero
  extractCost?: (json: any) => { costInDollars: number; inferenceId: string } | undefined
}
```

### 3.2 Implémentation pour TensorZero (oaCompatHelper)

**Fichier**: `packages/console/app/src/routes/zen/util/provider/openai-compatible.ts`

```typescript
export const oaCompatHelper: ProviderHelper = () => ({
  format: "oa-compat",
  // ...existing code...

  // NOUVEAU: Extract cost from TensorZero response
  extractCost: (json: any) => {
    const cost = json.usage?.cost
    if (cost === null || cost === undefined) return undefined

    return {
      costInDollars: cost, // Already in dollars from TensorZero
      inferenceId: json.inference_id,
    }
  },

  normalizeUsage: (usage: Usage) => {
    // ...existing code...
  },
})
```

### 3.3 Modification du Handler

**Fichier**: `packages/console/app/src/routes/zen/util/handler.ts`

#### 3.3.1 Nouvelle fonction: trackUsageTensorZero

```typescript
async function trackUsageTensorZero(
  sessionId: string,
  billingSource: BillingSource,
  authInfo: AuthInfo,
  modelInfo: ModelInfo,
  providerInfo: ProviderInfo,
  usageInfo: UsageInfo,
  tensorZeroCost: { costInDollars: number; inferenceId: string },
) {
  const { inputTokens, outputTokens, reasoningTokens, cacheReadTokens, cacheWrite5mTokens, cacheWrite1hTokens } =
    usageInfo
  const { costInDollars, inferenceId } = tensorZeroCost

  logger.metric({
    "tokens.input": inputTokens,
    "tokens.output": outputTokens,
    "cost.tensorzero": costInDollars,
    "inference.id": inferenceId,
  })

  if (billingSource === "anonymous") return

  // Convert dollars to microcents for balance
  const costInMicroCents = dollarsToMicroCents(costInDollars)

  await Database.use((db) =>
    Promise.all([
      // Insert usage record with TensorZero reference
      db.insert(UsageTable).values({
        workspaceID: authInfo.workspaceID,
        id: Identifier.create("usage"),
        model: modelInfo.id,
        provider: providerInfo.id,
        inputTokens,
        outputTokens,
        reasoningTokens,
        cacheReadTokens,
        cacheWrite5mTokens,
        cacheWrite1hTokens,
        costDollars: costInDollars.toString(),
        tensorzeroInferenceId: inferenceId,
        syncStatus: "pending",
        cost: costInMicroCents,
        keyID: authInfo.apiKeyId,
        sessionID: sessionId.substring(0, 30),
        enrichment: {
          plan: billingSource === "subscription" ? "sub" : billingSource === "lite" ? "lite" : "byok",
          source: "tensorzero",
          customerId: authInfo.workspaceID,
        },
      }),
      // Update key last used
      db
        .update(KeyTable)
        .set({ timeUsed: sql`now()` })
        .where(and(eq(KeyTable.workspaceID, authInfo.workspaceID), eq(KeyTable.id, authInfo.apiKeyId))),
      // Update balance (for balance-based billing)
      ...(billingSource === "balance"
        ? [
            db
              .update(BillingTable)
              .set({
                balance: sql`${BillingTable.balance} - ${costInMicroCents}`,
                monthlyUsage: sql`
                  CASE
                    WHEN MONTH(${BillingTable.timeMonthlyUsageUpdated}) = MONTH(now()) AND YEAR(${BillingTable.timeMonthlyUsageUpdated}) = YEAR(now()) THEN ${BillingTable.monthlyUsage} + ${costInMicroCents}
                    ELSE ${costInMicroCents}
                  END
                `,
                timeMonthlyUsageUpdated: sql`now()`,
              })
              .where(eq(BillingTable.workspaceID, authInfo.workspaceID)),
          ]
        : []),
    ]),
  )

  return { costInMicroCents, costInDollars }
}
```

#### 3.3.2 Integration dans le flux principal

```typescript
// Line ~215 dans handler.ts
const tensorZeroData = providerInfo.extractCost?.(json)

let costInfo: CostInfo | { totalCostInCent: number; costInDollars: number }

if (tensorZeroData) {
  // Use TensorZero cost - convert dollars to cents
  const costInCent = Math.round(tensorZeroData.costInDollars * 100)
  costInfo = { totalCostInCent: costInCent, costInDollars: tensorZeroData.costInDollars }

  // Track with TensorZero metadata
  await trackUsageTensorZero(sessionId, billingSource, authInfo!, modelInfo, providerInfo, usageInfo, tensorZeroData)
} else {
  // Fallback to internal calculation
  costInfo = calculateCost(modelInfo, usageInfo)
  await trackUsage(sessionId, billingSource, authInfo!, modelInfo, providerInfo, usageInfo, costInfo)
}

// Continue with reload check
await reload(billingSource, authInfo!, costInfo)
```

### 3.4 Utilitaires de Conversion

**Fichier**: `packages/console/core/src/util/price.ts`

```typescript
// Convert dollars to microcents (1 dollar = 100,000,000 microcents)
export const dollarsToMicroCents = (dollars: number): number => {
  return Math.round(dollars * 100_000_000)
}

// Convert microcents to dollars
export const microCentsToDollars = (microCents: number): number => {
  return microCents / 100_000_000
}

// Convert cents to microcents
export const centsToMicroCents = (cents: number): number => {
  return cents * 10_000
}
```

---

## 4. Job de Réconciliation

### 4.1 Script de Réconciliation Quotidienne

**Fichier**: `packages/console/core/script/reconcile-tensorzero.ts`

```typescript
import { Database, eq, sql, and, gte, lt } from "../drizzle"
import { BillingTable, UsageTable, TensorZeroCustomerTable } from "../schema/billing.sql"
import { Identifier } from "../identifier"
import { clickhouse } from "./clickhouse-client"

const RECONCILE_BATCH_SIZE = 1000

interface ClickHouseCost {
  customer_id: string
  date: string
  total_cost: number
  total_input_tokens: number
  total_output_tokens: number
  inference_count: number
}

async function reconcileBilling() {
  console.log("Starting TensorZero billing reconciliation...")

  const yesterday = new Date()
  yesterday.setDate(yesterday.getDate() - 1)
  const periodStart = yesterday.toISOString().split("T")[0]
  const periodEnd = new Date().toISOString().split("T")[0]

  // 1. Get all active TensorZero customers
  const customers = await Database.use((tx) =>
    tx
      .select({
        workspaceID: TensorZeroCustomerTable.workspaceID,
        customerId: TensorZeroCustomerTable.tensorzeroCustomerId,
      })
      .from(TensorZeroCustomerTable)
      .where(eq(TensorZeroCustomerTable.enabled, true)),
  )

  console.log(`Reconciling ${customers.length} customers for period ${periodStart}`)

  // 2. Query ClickHouse for costs per customer
  const clickhouseCosts = await queryClickHouseCosts(periodStart, periodEnd)

  // 3. Build map of costs by customer
  const costMap = new Map<string, ClickHouseCost>()
  for (const row of clickhouseCosts) {
    costMap.set(row.customer_id, row)
  }

  // 4. Reconcile each customer
  for (const customer of customers) {
    const chCost = costMap.get(customer.customerId)

    if (!chCost) {
      console.log(`No costs found for customer ${customer.customerId}`)
      continue
    }

    await reconcileCustomer(customer.workspaceID, customer.customerId, chCost)
  }

  // 5. Handle pending/inconsistent records
  await handlePendingRecords()

  console.log("Reconciliation complete.")
}

async function queryClickHouseCosts(startDate: string, endDate: string): Promise<ClickHouseCost[]> {
  const query = `
    SELECT
      tags['customer_id'] AS customer_id,
      toDate(timestamp) AS date,
      sum(cost) AS total_cost,
      sum(input_tokens) AS total_input_tokens,
      sum(output_tokens) AS total_output_tokens,
      count() AS inference_count
    FROM ModelInference
    WHERE timestamp >= '${startDate}' AND timestamp < '${endDate}'
      AND tags['customer_id'] IS NOT NULL
    GROUP BY customer_id, date
    ORDER BY customer_id, date
  `

  return await clickhouse.query(query, { format: "JSONEachRow" })
}

async function reconcileCustomer(workspaceID: string, customerId: string, chCost: ClickHouseCost) {
  // Get OpenCode recorded costs for this period
  const opencodeCosts = await Database.use((tx) =>
    tx
      .select({
        totalCost: sql`SUM(${UsageTable.cost})`,
        count: sql`COUNT(*)`,
      })
      .from(UsageTable)
      .where(and(eq(UsageTable.workspaceID, workspaceID), sql`DATE(${UsageTable.timeCreated}) = '${chCost.date}'`)),
  )

  const opencodeTotal = opencodeCosts[0]?.totalCost ?? 0
  const chTotalMicroCents = dollarsToMicroCents(chCost.total_cost)

  const discrepancy = Math.abs(opencodeTotal - chTotalMicroCents)
  const discrepancyPercent = opencodeTotal > 0 ? (discrepancy / opencodeTotal) * 100 : 0

  if (discrepancyPercent > 0.1) {
    // > 0.1% discrepancy
    console.log(
      `⚠️ Discrepancy for ${customerId}: ` +
        `OpenCode=${microCentsToDollars(opencodeTotal).toFixed(6)} ` +
        `ClickHouse=${chCost.total_cost.toFixed(6)} ` +
        `Diff=${discrepancyPercent.toFixed(2)}%`,
    )

    // Adjust balance based on ClickHouse (source of truth)
    const adjustment = chTotalMicroCents - opencodeTotal

    await Database.use((tx) =>
      tx
        .update(BillingTable)
        .set({
          balance: sql`${BillingTable.balance} + ${adjustment}`,
        })
        .where(eq(BillingTable.workspaceID, workspaceID)),
    )

    // Mark all records for this period as reconciled
    await Database.use((tx) =>
      tx
        .update(UsageTable)
        .set({
          reconciledCostDollars: chCost.total_cost.toString(),
          reconciledAt: sql`NOW()`,
          syncStatus: "discrepancy",
        })
        .where(and(eq(UsageTable.workspaceID, workspaceID), sql`DATE(${UsageTable.timeCreated}) = '${chCost.date}'`)),
    )
  } else {
    // Mark as synced
    await Database.use((tx) =>
      tx
        .update(UsageTable)
        .set({
          reconciledCostDollars: chCost.total_cost.toString(),
          reconciledAt: sql`NOW()`,
          syncStatus: "synced",
        })
        .where(and(eq(UsageTable.workspaceID, workspaceID), sql`DATE(${UsageTable.timeCreated}) = '${chCost.date}'`)),
    )
  }
}

async function handlePendingRecords() {
  // Find records that exist in OpenCode but not in ClickHouse (possible fraud)
  const pending = await Database.use((tx) =>
    tx
      .select()
      .from(UsageTable)
      .where(
        and(eq(UsageTable.syncStatus, "pending"), sql`${UsageTable.timeCreated} < DATE_SUB(NOW(), INTERVAL 2 DAY)`),
      )
      .limit(100),
  )

  for (const record of pending) {
    if (record.tensorzeroInferenceId) {
      // Query TensorZero directly for this inference
      const chRecord = await queryClickHouseInference(record.tensorzeroInferenceId)

      if (!chRecord) {
        console.log(`⚠️ Inference ${record.tensorzeroInferenceId} not found in TensorZero - possible fraud`)
        // Could flag for manual review or auto-suspend
      }
    }
  }
}

async function queryClickHouseInference(inferenceId: string): Promise<any | null> {
  const query = `
    SELECT * FROM ModelInference
    WHERE inference_id = '${inferenceId}'
    LIMIT 1
  `
  const results = await clickhouse.query(query, { format: "JSONEachRow" })
  return results[0] ?? null
}

// Helper
function dollarsToMicroCents(dollars: number): number {
  return Math.round(dollars * 100_000_000)
}

function microCentsToDollars(microCents: number): number {
  return microCents / 100_000_000
}

// Run
reconcileBilling()
  .then(() => process.exit(0))
  .catch((e) => {
    console.error(e)
    process.exit(1)
  })
```

### 4.2 Client ClickHouse

**Fichier**: `packages/console/core/script/clickhouse-client.ts`

```typescript
import { createClient } from "@clickhouse/client"

export const clickhouse = createClient({
  url: process.env.TENSORZERO_CLICKHOUSE_URL ?? "http://localhost:8123",
  username: process.env.TENSORZERO_CLICKHOUSE_USERNAME ?? "default",
  password: process.env.TENSORZERO_CLICKHOUSE_PASSWORD ?? "",
  database: process.env.TENSORZERO_CLICKHOUSE_DATABASE ?? "tensorzero",
  request_timeout: 60000,
  max_open_connections: 10,
})
```

---

## 5. API de Gestion

### 5.1 Endpoints d'Admin

**Fichier**: `packages/console/app/src/routes/api/billing/tensorzero.ts`

```typescript
import { z } from "zod"
import { fn } from "@opencode-ai/console-core/fn"
import { Database, eq } from "@opencode-ai/console-core/drizzle"
import { TensorZeroCustomerTable, UsageTable } from "@opencode-ai/console-core/schema/billing.sql"

export const tensorzeroRouter = {
  // Register a customer for TensorZero billing
  registerCustomer: fn(
    z.object({
      tensorzeroCustomerId: z.string(),
    }),
    async ({ tensorzeroCustomerId }) => {
      const workspaceID = Actor.workspace()

      await Database.use((tx) =>
        tx
          .insert(TensorZeroCustomerTable)
          .values({
            workspaceID,
            id: Identifier.create("tzc"),
            tensorzeroCustomerId,
            enabled: true,
          })
          .onDuplicateKeyUpdate({
            tensorzeroCustomerId,
          }),
      )

      return { success: true }
    },
  ),

  // Get reconciliation status
  getReconciliationStatus: fn(
    z.object({
      days: z.number().default(7),
    }),
    async ({ days }) => {
      const workspaceID = Actor.workspace()

      const stats = await Database.use((tx) =>
        tx
          .select({
            total: sql`COUNT(*)`,
            synced: sql`SUM(CASE WHEN ${UsageTable.syncStatus} = 'synced' THEN 1 ELSE 0 END)`,
            pending: sql`SUM(CASE WHEN ${UsageTable.syncStatus} = 'pending' THEN 1 ELSE 0 END)`,
            discrepancy: sql`SUM(CASE WHEN ${UsageTable.syncStatus} = 'discrepancy' THEN 1 ELSE 0 END)`,
            totalCost: sql`SUM(${UsageTable.costDollars})`,
          })
          .from(UsageTable)
          .where(
            and(
              eq(UsageTable.workspaceID, workspaceID),
              sql`${UsageTable.timeCreated} >= DATE_SUB(NOW(), INTERVAL ${days} DAY)`,
            ),
          ),
      )

      return stats[0]
    },
  ),

  // Manual reconciliation trigger
  triggerReconciliation: fn(
    z.object({
      startDate: z.string(),
      endDate: z.string(),
    }),
    async ({ startDate, endDate }) => {
      // This would trigger the reconciliation job for specific dates
      // Could be implemented as a background job queue
      return { jobId: Identifier.create("reconcile") }
    },
  ),
}
```

---

## 6. Migration

### 6.1 Stratégie de Migration

```
Phase 1: Parallel Recording (Semaine 1-2)
├── Enregistrer les deux coûts (interne + TensorZero)
├── Comparer quotidiennement
└── Valider que les coûts correspondent

Phase 2: Switch to TensorZero (Semaine 3)
├── Activer le nouveau flux
├── Désactiver le calcul interne
└── Vérifier le reload Stripe

Phase 3: Reconciliation (Continu)
├── Job quotidien
├── Monitoring des écarts
└── Alertes si > 1% écart
```

### 6.2 Script de Migration

```typescript
// Migration: Add new columns to UsageTable
// This would be run via drizzle migrations

import { sql } from "drizzle-orm"
import { mysqlTable, varchar, decimal, datetime, enum } from "drizzle-orm/mysql-core"

// Add columns (migration file)
export async function up() {
  // Add cost_dollars column
  await sql`ALTER TABLE usage ADD COLUMN cost_dollars DECIMAL(20, 10) AFTER cost`

  // Add tensorzero_inference_id
  await sql`ALTER TABLE usage ADD COLUMN tensorzero_inference_id VARCHAR(36) AFTER cost_dollars`

  // Add reconciled columns
  await sql`ALTER TABLE usage ADD COLUMN reconciled_cost_dollars DECIMAL(20, 10) AFTER tensorzero_inference_id`
  await sql`ALTER TABLE usage ADD COLUMN reconciled_at DATETIME(3) AFTER reconciled_cost_dollars`
  await sql`ALTER TABLE usage ADD COLUMN sync_status ENUM('pending', 'synced', 'discrepancy') DEFAULT 'pending' AFTER reconciled_at`

  // Add indexes
  await sql`ALTER TABLE usage ADD INDEX idx_usage_tensorzero_id (tensorzero_inference_id)`
  await sql`ALTER TABLE usage ADD INDEX idx_usage_sync_status (sync_status)`

  // Create tensorzero_customer table
  await sql`
    CREATE TABLE IF NOT EXISTS tensorzero_customer (
      workspace_id VARCHAR(30) NOT NULL,
      id VARCHAR(30) NOT NULL,
      created_at DATETIME(3) DEFAULT CURRENT_TIMESTAMP(3),
      updated_at DATETIME(3) DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
      tensorzero_customer_id VARCHAR(255) NOT NULL,
      last_sync_at DATETIME(3),
      enabled BOOLEAN DEFAULT TRUE,
      UNIQUE KEY tz_customer_unique (tensorzero_customer_id),
      INDEX idx_workspace_id (workspace_id)
    ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4
  `
}

export async function down() {
  await sql`ALTER TABLE usage DROP COLUMN cost_dollars`
  await sql`ALTER TABLE usage DROP COLUMN tensorzero_inference_id`
  await sql`ALTER TABLE usage DROP COLUMN reconciled_cost_dollars`
  await sql`ALTER TABLE usage DROP COLUMN reconciled_at`
  await sql`ALTER TABLE usage DROP COLUMN sync_status`
  await sql`DROP TABLE IF EXISTS tensorzero_customer`
}
```

---

## 7. Variables d'Environnement

```bash
# TensorZero ClickHouse
TENSORZERO_CLICKHOUSE_URL=http://localhost:8123
TENSORZERO_CLICKHOUSE_USERNAME=default
TENSORZERO_CLICKHOUSE_PASSWORD=
TENSORZERO_CLICKHOUSE_DATABASE=tensorzero

# Reconciliation Job
RECONCILE_CRON_SCHEDULE="0 2 * * *"  # Daily at 2am
RECONCILE_DISCREPANCY_THRESHOLD=0.01  # 1% threshold
```

---

## 8. Monitoring & Alertes

### 8.1 Métriques à Surveiller

| Métrique                             | Seuil d'alerte   |
| ------------------------------------ | ---------------- |
| `reconciliation.discrepancy_percent` | > 1%             |
| `reconciliation.pending_count`       | > 100            |
| `billing.balance`                    | < reload trigger |
| `tensorzero.inference.missing`       | > 0              |

### 8.2 Dashboard

Créer un dashboard avec:

- Balance par workspace
- Coût journalier (TensorZero vs OpenCode)
- Nombre d'inférences pendantes
- Statut de réconciliation

---

## 9. Ordre d'Implémentation

```
1. Database Schema
   └── Exécuter migration
   └── Ajouter colonnes UsageTable
   └── Créer TensorZeroCustomerTable

2. Code Handler
   └── Ajouter extractCost au provider
   └── Modifier trackUsage pour TensorZero
   └── Intégrer dans flux principal

3. Client ClickHouse
   └── Créer clickhouse-client.ts
   └── Tester connexion

4. Job de Réconciliation
   └── Implémenter reconcile-tensorzero.ts
   └── Ajouter cron job

5. API & Monitoring
   └── Ajouter endpoints admin
   └── Créer dashboard

6. Migration
   └── Phase 1: Parallel
   └── Phase 2: Switch
   └── Phase 3: Production
```

---

## 10. Tests

### 10.1 Tests Unitaires

```typescript
// Test conversion utilities
describe("dollarsToMicroCents", () => {
  it("converts 0.001 dollars correctly", () => {
    expect(dollarsToMicroCents(0.001)).toBe(100_000)
  })

  it("handles zero", () => {
    expect(dollarsToMicroCents(0)).toBe(0)
  })
})

// Test extractCost
describe("extractCost", () => {
  it("extracts cost from TensorZero response", () => {
    const result = extractCost({
      inference_id: "123e4567-e89b-12d3-a456-426614174000",
      usage: { cost: 0.0025 },
    })

    expect(result).toEqual({
      costInDollars: 0.0025,
      inferenceId: "123e4567-e89b-12d3-a456-426614174000",
    })
  })

  it("returns undefined when cost is null", () => {
    const result = extractCost({
      inference_id: "123e4567-e89b-12d3-a456-426614174000",
      usage: {},
    })

    expect(result).toBeUndefined()
  })
})
```

### 10.2 Tests d'Intégration

```typescript
describe("TensorZero Billing Integration", () => {
  it("tracks usage with TensorZero cost", async () => {
    const mockResponse = {
      inference_id: "test-uuid",
      usage: { cost: 0.01, input_tokens: 100, output_tokens: 50 },
    }

    // Track usage
    await trackUsageTensorZero("session-123", "balance", mockAuthInfo, mockModelInfo, mockProviderInfo, mockUsageInfo, {
      costInDollars: 0.01,
      inferenceId: "test-uuid",
    })

    // Verify balance was debited
    const billing = await Billing.get()
    expect(billing.balance).toBeLessThan(initialBalance)
  })

  it("reconciles costs with ClickHouse", async () => {
    // Setup: add test records
    await addTestUsage("tz-123", 0.01)

    // Run reconciliation
    await reconcileBilling()

    // Verify sync status
    const usage = await getUsage("tz-123")
    expect(usage.syncStatus).toBe("synced")
  })
})
```

---

## Résumé

Cette implémentation délègue complètement le metering à TensorZero/ClickHouse:

| Composant           | Description                  |
| ------------------- | ---------------------------- |
| **Source of Truth** | TensorZero ClickHouse        |
| **Coût**            | Dollars (précision décimale) |
| **Réconciliation**  | Quotidienne, automatique     |
| **Balance**         | Débitée en temps réel        |
| **Fraude**          | Détectée via réconciliation  |

Les avantages:

- Plus de calcul de coût local
- Précision garantie par TensorZero
- Réconciliation automatique des écarts
- Prêt pour le multi-tenant avec tags
