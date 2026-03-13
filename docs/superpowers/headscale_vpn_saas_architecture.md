# Architecture SaaS OpenCode avec Headscale VPN

**Analyse complète basée sur le code source**

---

## 1. Analyse de l'Architecture Actuelle

### 1.1 Flux de Connexion Actuel

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Desktop Client │────▶│  OpenCode Server │────▶│  Console API    │
│  (Electron)     │     │  (localhost)     │     │  (Cloud)        │
└─────────────────┘     └──────────────────┘     └─────────────────┘
        │                        │                        │
        │ spawn CLI             │ x-api-key header       │
        │ localhost:PORT        │                        │
        │ password auth         │                        │
```

**Composants actuels** :

| Composant        | Fichier                                     | Rôle                         |
| ---------------- | ------------------------------------------- | ---------------------------- |
| Desktop Electron | `packages/desktop-electron/src/main/cli.ts` | Spawn le serveur local       |
| Server           | `packages/opencode/src/server/server.ts`    | API locale (Hono)            |
| Console API      | `packages/console/app/src/routes/zen/`      | Auth, billing, workspaces    |
| Auth             | `handler.ts:456-578`                        | Validate API key → Workspace |

### 1.2 Authentication Actuelle

```typescript
// handler.ts - authenticate()
const apiKey = opts.parseApiKey(input.request.headers) // x-api-key header
// Query KeyTable pour trouver workspace
const data = await tx
  .select()
  .from(KeyTable)
  .where(and(eq(KeyTable.key, apiKey), isNull(KeyTable.timeDeleted)))
```

**Clés d'API** :

- Format : `sk-` + 64 caractères aléatoires
- Stockées dans `KeyTable`
- Liées à un `workspaceID` et `userID`

### 1.3 Architecture Billing Actuel

```
API Key ──▶ Workspace ──▶ BillingTable.balance ──▶ Stripe
                │
                └──▶ UsageTable ──▶ Coût calculé localement
```

---

## 2. Architecture Cible avec Headscale VPN

### 2.1 Vue d'Ensemble

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           User's Machine                                    │
│  ┌─────────────────┐                                                       │
│  │  Desktop Client │◀── VPN (Headscale)                                    │
│  │  (Electron)     │     │                                                  │
│  └────────┬────────┘     │    IP: 100.x.y.z (private VPN)                 │
│           │              │                                                  │
│           │ spawn        │                                                  │
│           ▼              │                                                  │
│  ┌─────────────────┐     │    ┌─────────────────────────────────────────┐  │
│  │  OpenCode Server│     │    │           Headscale Network            │  │
│  │  (local/remote) │     │    │                                         │  │
│  └────────┬────────┘     │    │   VPN Server (your infrastructure)     │  │
│           │              │    │   - OpenCode Server                    │  │
│           │ API call     │    │   - TensorZero Gateway                 │  │
│           └──────────────┼────┼──▶- ClickHouse                         │  │
│                          │    │   - MySQL                              │  │
│                          │    │   - Redis (sessions)                   │  │
│                          │    └─────────────────────────────────────────┘  │
│                          │                                                  │
│                          ▼                                                  │
│                 ┌──────────────────┐                                       │
│                 │   Console API    │  (optional - can be same infra)       │
│                 │   (Cloud/Local) │                                       │
│                 └──────────────────┘                                       │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.2 Variantes d'Architecture

#### Variante A : Tout en VPN (Recommandée)

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Infrastructure (VPS/Dedicated)                      │
│                                                                             │
│  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐      │
│  │  Headscale │   │   OpenCode  │   │ TensorZero  │   │   MySQL     │      │
│  │  VPN       │   │   Server    │   │  Gateway   │   │  Database   │      │
│  │  (100.x)   │   │  (console) │   │  (CH)      │   │             │      │
│  └──────┬──────┘   └──────┬──────┘   └──────┬──────┘   └──────┬──────┘      │
│         │                 │                 │                 │             │
│         │         ┌───────┴───────┐        │                 │             │
│         │         │  Shared       │        │                 │             │
│         │         │  Network     │◀───────┴─────────────────┘             │
│         │         │              │                                        │
│         │         └──────────────┘                                        │
│         │                                                                  │
│  ┌──────┴──────┐                                                         │
│  │   Users     │  Se connectent via VPN                                   │
│  │  (Desktop)  │  IP: 100.x.y.z (attribué par Headscale)                 │
│  └─────────────┘                                                         │
└─────────────────────────────────────────────────────────────────────────────┘
```

#### Variante B : Hybrid Cloud

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  User Machine                     │              Cloud Provider            │
│                                   │                                        │
│  ┌─────────────┐                  │    ┌─────────────────────────────┐    │
│  │ Desktop     │───VPN (tunnel)───│───▶│  Console API + Billing      │    │
│  │ Client     │                  │    │  (Stripe, Web UI, etc.)     │    │
│  └─────────────┘                  │    └──────────────┬──────────────┘    │
│                                   │                   │                   │
│                                   │    ┌──────────────┴──────────────┐    │
│                                   │    │  OpenCode Server +         │    │
│                                   │    │  TensorZero Gateway       │    │
│                                   │    │  (via VPN tunnel)         │    │
│                                   │    └───────────────────────────┘    │
│                                   │                                        │
└───────────────────────────────────┴────────────────────────────────────────┘
```

---

## 3. Intégration Headscale

### 3.1 Configuration Headscale

```yaml
# headscale/config.yaml
server_url: https://your-vpn.example.com
listen_addr: 0.0.0.0:8080
metrics_listen_addr: 127.0.0.1:9090

grpc_listen_addr: 0.0.0.0:50443
grpc_allow_insecure: false

private_key_path: /var/lib/headscale/private.key
noise:
  private_key_path: /var/lib/headscale/noise_private.key

derp:
  server:
    enabled: false

disable_check_updates: false
ephemeral_node_inactivity_timeout: 30m
node_update_check_interval: 10s

db_type: sqlite3
db_path: /var/lib/headscale/db.sqlite

acme_url: https://acme-v02.api.letsencrypt.org/directory
acme_email: ""
tls_letsencrypt_hostname: ""
tls_cert_path: ""
tls_key_path: ""

log:
  format: text
  level: info

dns_config:
  override_local_dns: true
  nameservers:
    - 1.1.1.1
  domains: []
  magic_dns: true
  base_domain: internal.example.com

unix_socket: /var/run/headscale/headscale.sock
unix_socket_permission: "0770"

logtail:
  enabled: false

randomize_client_port: false
```

### 3.2 Nom Machines dans le VPN

Chaque instance OpenCode Server obtient une IP VPN :

```
100.64.1.10  -  opencode-server-01.internal.example.com
100.64.1.11  -  opencode-server-02.internal.example.com
...
```

### 3.3 Configuration Desktop Client

Le desktop client doit se connecter au serveur VPN et utiliser l'IP VPN :

```typescript
// packages/desktop-electron/src/main/server.ts
export function getServerUrl(): string {
  const vpnIP = getHeadscaleIP() // 100.64.x.x
  const port = getConfiguredPort()
  return `http://${vpnIP}:${port}`
}

export function connectToVPN() {
  // Utiliser le CLI headscale ou un wrapper
  exec("headscale nodes register --key <node-key>")
}
```

### 3.4 Authentication via VPN IP

Au lieu d'API keys, on peut authentifier par IP VPN :

```typescript
// handler.ts - authenticate()
async function authenticateVPN(modelInfo: ModelInfo) {
  const clientIP = input.request.headers.get("x-forwarded-for") ?? input.request.headers.get("x-real-ip")

  // Vérifier que l'IP est dans le range VPN Headscale
  if (!isVPNIP(clientIP)) {
    throw new AuthError("Access denied: Not in VPN")
  }

  // Mapper IP → Workspace
  const workspace = await lookupWorkspaceByVPNIP(clientIP)
  if (!workspace) {
    throw new AuthError("No workspace for this VPN IP")
  }

  return {
    workspaceID: workspace.id,
    billing: workspace.billing,
    // ...
  }
}
```

---

## 4. Modifications Requises

### 4.1 Schema : Ajouter IP VPN

```typescript
// packages/console/core/src/schema/billing.sql.ts
export const WorkspaceTable = mysqlTable(
  "workspace",
  {
    ...workspaceColumns,
    // ... existing fields

    // NOUVEAU: IP VPN assignée
    vpnIP: varchar("vpn_ip", { length: 15 }),
    vpnNodeID: varchar("vpn_node_id", { length: 255 }),

    // NOUVEAU: Configuration serveur
    serverHostname: varchar("server_hostname", { length: 255 }),
    serverPort: int("server_port"),
  },
  // ...
)
```

### 4.2 API : Auth par VPN IP

```typescript
// packages/console/app/src/routes/zen/util/handler.ts

type AuthMode = "api-key" | "vpn-ip"

async function authenticate(modelInfo: ModelInfo, authMode: AuthMode) {
  if (authMode === "vpn-ip") {
    return authenticateByVPNIP(modelInfo)
  }
  return authenticateByAPIKey(modelInfo)
}

async function authenticateByVPNIP(modelInfo: ModelInfo) {
  const ip = getClientRealIP(input.request.headers)

  // Vérifier range VPN (100.64.0.0/10)
  if (!isHeadscaleIP(ip)) {
    throw new AuthError("Not in VPN")
  }

  const workspace = await Database.use((tx) =>
    tx
      .select()
      .from(WorkspaceTable)
      .where(eq(WorkspaceTable.vpnIP, ip))
      .then((rows) => rows[0]),
  )

  if (!workspace) {
    throw new AuthError("No workspace for this IP")
  }

  // Retourner les infos de billing
  const billing = await getBillingForWorkspace(workspace.id)

  return {
    workspaceID: workspace.id,
    billing,
    // pas de apiKeyId car authentifié par IP
  }
}
```

### 4.3 Server: Supprimer Auth Basic

```typescript
// packages/opencode/src/server/server.ts

// Modifier pour accepter les connexions VPN sans mot de passe
// (ou avec un token de session)
export const createApp = (opts: {
  cors?: string[]
  requireAuth?: boolean // nouveau paramètre
}): Hono => {
  const app = new Hono()

  return app.use((c, next) => {
    // Skip basic auth pour les connexions VPN locales
    const isLocal = c.req.header("x-forwarded-for")?.startsWith("100.")
    if (isLocal && !Flag.OPENCODE_SERVER_PASSWORD) {
      return next()
    }
    // Sinon, require auth
    // ...
  })
}
```

### 4.4 Client: Connexion VPN Automatique

```typescript
// packages/desktop-electron/src/main/vpn.ts

import { exec } from "child_process"
import { promisify } from "util"

const execAsync = promisify(exec)

export class HeadscaleManager {
  private nodeKey: string
  private serverURL: string

  constructor(nodeKey: string, serverURL: string) {
    this.nodeKey = nodeKey
    this.serverURL = serverURL
  }

  async register(): Promise<string> {
    // Enregistrer le node avec Headscale
    const { stdout } = await execAsync(`headscale nodes register --key ${this.nodeKey}`, {
      cwd: this.getHeadscaleDir(),
    })

    // Retourne l'IP VPN assignée
    return this.getVPNIP()
  }

  async getVPNIP(): Promise<string> {
    const { stdout } = await execAsync(`headscale nodes list --output json`, { cwd: this.getHeadscaleDir() })
    const nodes = JSON.parse(stdout)
    const currentNode = nodes.find((n) => n.id === this.nodeKey)
    return currentNode?.ip_addresses?.[0]
  }

  async waitForVPN(): Promise<void> {
    // Attendre que l'interface VPN soit UP
    let connected = false
    while (!connected) {
      await new Promise((r) => setTimeout(r, 1000))
      try {
        const { stdout } = await execAsync("ip addr show tailscale0")
        connected = stdout.includes("state UP")
      } catch {
        connected = false
      }
    }
  }

  private getHeadscaleDir(): string {
    return path.join(app.getPath("userData"), "headscale")
  }
}
```

---

## 5. Intégration TensorZero Billing (Full ClickHouse)

### 5.1 Architecture Complète

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Infrastructure                                      │
│                                                                             │
│  ┌─────────────┐   ┌─────────────────┐   ┌──────────────────────────┐     │
│  │  Headscale │   │  OpenCode Server │   │   TensorZero Gateway      │     │
│  │  VPN       │   │                 │   │                          │     │
│  │  100.x.y.z │   │  - API handlers │   │  - Router vers providers│     │
│  └─────┬───────┘   │  - Auth (VPN)  │   │  - Cost tracking         │     │
│        │           │  - Billing      │   │  - ClickHouse write      │     │
│        │           └────────┬────────┘   └────────────┬─────────────┘     │
│        │                    │                         │                   │
│        │           ┌────────▼────────┐        ┌───────▼─────────┐       │
│        │           │   MySQL        │        │   ClickHouse     │       │
│        │           │                 │        │                  │       │
│        │           │ - Workspaces    │        │ - ModelInference │       │
│        │           │ - Keys          │        │   (cost, tokens) │       │
│        │           │ - Billing       │        │ - Tags: customer │       │
│        │           │ - Usage         │        │                  │       │
│        │           └─────────────────┘        └──────────────────┘       │
│        │                                                                │
│  ┌─────┴──────┐                                                         │
│  │  Desktop   │  Se connecte via VPN                                    │
│  │  Client    │  IP: 100.x.y.z                                         │
│  └────────────┘                                                         │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 5.2 Flow de Billing

```
1. Desktop Client ──▶ OpenCode Server (via VPN)
                         │
2. Server ──▶ TensorZero Gateway
                  │
3. TensorZero ──▶ Providers (OpenAI, Anthropic, etc.)
                  │
4. Response ◀── TensorZero
                  │
5. Server calcule/extrae cost
                  │
6. trackUsage() ──▶ MySQL (UsageTable)
                  │     └─ tensorzero_inference_id
                  │
7. Réconciliation quotidienne (cron)
                  │
8. ClickHouse ──▶ Query cost par customer_id
                  │
9. MySQL ──▶ Ajuste balance si écart
                  │
10. Si balance < trigger ──▶ Stripe reload
```

---

## 6.清单 des Modifications

### 6.1 Backend (OpenCode Server)

| Fichier                 | Modification                                                                        |
| ----------------------- | ----------------------------------------------------------------------------------- |
| `schema/billing.sql.ts` | Ajouter `vpnIP`, `vpnNodeID`, `serverHostname`, `serverPort`                        |
| `schema/billing.sql.ts` | Ajouter `tensorzeroInferenceId`, `reconciledCostDollars`, `syncStatus` à UsageTable |
| `handler.ts`            | Nouvelle fonction `authenticateByVPNIP()`                                           |
| `handler.ts`            | Intégrer `extractCost` pour TensorZero                                              |
| `billing.ts`            | Ajouter `trackUsageTensorZero()`                                                    |

### 6.2 Infrastructure

| Composant  | Action                                    |
| ---------- | ----------------------------------------- |
| Headscale  | Déployer serveur + config                 |
| ClickHouse | Configurer TensorZero + index customer_id |
| MySQL      | Migration schema                          |
| Redis      | Sessions (optionnel)                      |

### 6.3 Desktop Client

| Fichier     | Modification                         |
| ----------- | ------------------------------------ |
| `cli.ts`    | Ajouter connexion Headscale          |
| `server.ts` | Utiliser IP VPN au lieu de localhost |
| `vpn.ts`    | Nouvelle classe HeadscaleManager     |

---

## 7. Security Considerations

### 7.1 Authentication Multi-Couches

```typescript
// Niveaux d'auth (du plus faible au plus fort)

enum AuthLevel {
  NONE = 0, // Pas d'auth (dev only)
  VPN = 1, // IP dans VPN Headscale
  API_KEY = 2, // Clé API explicite
  OAUTH = 3, // OAuth (GitHub, Google)
}

function getRequiredAuthLevel(route: string): AuthLevel {
  if (route.startsWith("/health")) return AuthLevel.NONE
  if (route.startsWith("/internal")) return AuthLevel.VPN
  if (route.startsWith("/api")) return AuthLevel.API_KEY
  return AuthLevel.OAUTH
}
```

### 7.2 Rate Limiting par IP VPN

```typescript
// Limiter par IP VPN plutôt que par API key
const rateLimiter = createRateLimiter(
  modelInfo.id,
  rateLimit,
  clientVPNIP, // IP VPN au lieu de IP classique
)
```

### 7.3 Isolation entre Customers

```
┌─────────────────────────────────────────────────────────────────┐
│                      Headscale Network                          │
│                                                                 │
│  100.64.1.0/24  ──▶ Customer A                                │
│     │                                                         │
│  100.64.2.0/24  ──▶ Customer B                                │
│     │                                                         │
│  100.64.3.0/24  ──▶ Customer C                                │
│                                                                 │
│  ACLs Headscale:                                               │
│  - Customer A ne peut pas voir les IPs de B et C              │
│  - Accès only to shared services (OpenCode Server)             │
└─────────────────────────────────────────────────────────────────┘
```

---

## 8. Coûts et Infrastructure

### 8.1 Serveurs Recommandés

| Service         | Spec Minimum     | Recommandé             |
| --------------- | ---------------- | ---------------------- |
| Headscale       | 2 vCPU, 2GB RAM  | 2 vCPU, 4GB RAM        |
| OpenCode Server | 4 vCPU, 8GB RAM  | 8 vCPU, 16GB RAM       |
| TensorZero      | 2 vCPU, 4GB RAM  | 4 vCPU, 8GB RAM        |
| MySQL           | 2 vCPU, 4GB RAM  | 4 vCPU, 8GB RAM (SSD)  |
| ClickHouse      | 4 vCPU, 16GB RAM | 8 vCPU, 32GB RAM (SSD) |

### 8.2 Estimation Mensuelle (AWS/GCP)

| Service         | Coût估计           |
| --------------- | ------------------ |
| Headscale       | $20-40             |
| OpenCode Server | $80-160            |
| TensorZero      | $40-80             |
| MySQL (RDS)     | $50-100            |
| ClickHouse      | $100-200           |
| **Total**       | **$290-580/month** |

---

## 9. Prochaines Étapes

1. **Déployer Headscale**
   - Configurer serveur
   - Tester connexion

2. **Modifier schema MySQL**
   - Ajouter colonnes VPN
   - Ajouter colonnes TensorZero

3. **Adapter authentication**
   - Implémenter `authenticateByVPNIP()`
   - Tester avec desktop client

4. **Intégrer TensorZero**
   - Déployer gateway
   - Configurer cost tracking
   - Implémenter réconciliation

5. **Tester end-to-end**
   - Desktop → VPN → Server → TensorZero → Billing

---

## 10. Résumé

Cette architecture offre :

| Avantage         | Description                                  |
| ---------------- | -------------------------------------------- |
| **Sécurité**     | Isolation réseau par VPN                     |
| **Simplicité**   | Pas de management d'API keys côté client     |
| **Facturation**  | TensorZero/ClickHouse comme source de vérité |
| **Multi-tenant** | Chaque customer dans son subnet VPN          |
| **Contrôle**     | IP fixe par workspace                        |

**Inconvénients** :

- Dépendance à Headscale
- Latence potentielle (tunnel VPN)
- Plus complexe à débugger
