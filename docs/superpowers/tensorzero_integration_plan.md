# Adaptation du Billing pour TensorZero

Pour passer du calcul interne d'OpenCode à un calcul délégué à TensorZero, plusieurs modifications sont nécessaires dans le [handler](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/app/src/routes/zen/util/handler.ts#57-1019) de l'API Console.

## Modifications Proposées

### 1. Extension du Provider Helper (`packages/console/util/provider/provider.ts`)

Il faut permettre aux aides de provider d'extraire le coût directement depuis la réponse du proxy.

```typescript
// Ajouter une méthode optionnelle dans l'interface ProviderHelper
export interface ProviderHelper {
  // ... existing methods
  extractCost?: (json: any, headers: Headers) => number | undefined;
}
```

### 2. Adaptation de l'[oaCompatHelper](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/app/src/routes/zen/util/provider/openai-compatible.ts#24-75) (`packages/console/util/provider/openai-compatible.ts`)

TensorZero peut être configuré pour renvoyer le coût dans les extensions de la réponse ou dans les en-têtes.

```typescript
export const oaCompatHelper: ProviderHelper = () => ({
  // ...
  extractCost: (json: any, headers: Headers) => {
    // Exemple : TensorZero renvoie souvent le coût dans un champ personnalisé ou header
    // Si TensorZero est configuré pour renvoyer le coût total :
    if (json.usage?.total_cost) return json.usage.total_cost * 100; // Conversion en cents
    
    const costHeader = headers.get("x-tensorzero-cost");
    if (costHeader) return parseFloat(costHeader) * 100;
    
    return undefined;
  }
});
```

### 3. Mise à jour du Handler ([packages/console/app/src/routes/zen/util/handler.ts](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/app/src/routes/zen/util/handler.ts))

Le handler doit utiliser le coût extrait s'il existe, sinon retomber sur le calcul interne.

```typescript
// Lignes ~215-216
const usageInfo = providerInfo.normalizeUsage(json.usage);

// Utiliser le coût de TensorZero si disponible, sinon calcul interne
const tensorZeroCost = providerInfo.extractCost?.(json, res.headers);
const costInfo = tensorZeroCost 
  ? { totalCostInCent: tensorZeroCost, /* ... autres champs optionnels */ }
  : calculateCost(modelInfo, usageInfo);
```

### 4. Précision sur les Microcents et Unités

Le système OpenCode utilise des **microcents** (1 cent = 1,000,000 microcents). Si TensorZero renvoie un coût en dollars (ex: `0.002`), la conversion est critique :

```typescript
// Dans trackUsage de handler.ts
const costInMicroCents = tensorZeroCostInDollars * 100_000_000; 
// 0.002 * 10^8 = 200,000 microcents (soit 0.2 cents)
```

### 5. Interaction avec le Système de Reload (Stripe Invoicing)

La fonction [reload](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/core/src/billing.ts#67-128) dans [handler.ts](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/app/src/routes/zen/util/handler.ts) (ligne 989) se base sur la balance mise à jour dans [trackUsage](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/app/src/routes/zen/util/handler.ts#813-988). 

- **Bonne nouvelle** : Si vous injectez le coût de TensorZero dans [trackUsage](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/app/src/routes/zen/util/handler.ts#813-988), la balance diminuera en conséquence. Si elle passe sous le `reloadTrigger`, la fonction `Billing.reload()` sera appelée automatiquement.
- **Continuité Stripe** : `Billing.reload()` continuera de créer une facture Stripe et de charger la carte de l'utilisateur. Le lien entre l'usage (dicté par TensorZero) et le paiement (exécuté par Stripe) reste totalement fonctionnel sans modifier [billing.ts](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/core/src/billing.ts).

## Stratégie de Réconciliation

Pour assurer la cohérence entre les factures Stripe et l'usage TensorZero :

1.  **Passage de l'ID de requête** : Dans [handler.ts](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/app/src/routes/zen/util/handler.ts), assurez-vous d'enregistrer l'ID d'inférence de TensorZero dans le champ `UsageTable.sessionID` ou `enrichment`.
2.  **Webhooks Stripe** : Aucune modification nécessaire dans [stripe/webhook.ts](file:///c:/Users/NMarchitecte/Documents/opencode/packages/console/app/src/routes/stripe/webhook.ts). Le système continuera d'augmenter la balance `BillingTable.balance` dès qu'un paiement Stripe réussit (Checkout ou Reload).

> [!IMPORTANT]
> Le rôle de Stripe est de gérer le **"Portefeuille"** (Wallet), tandis que TensorZero devient le **"Compteur"** (Meter). L'adaptation se fait uniquement au point de rencontre : le débit de la balance.
