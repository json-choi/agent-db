// Provider capabilities are data, not UI conditionals. Unsupported adapters stay
// visible as planned but can never be selected for managed credential issuance.

export const providerKinds = [
  "awsRds",
  "gcpCloudSql",
  "oracleOci",
  "neon",
  "planetScale",
  "mongodbAtlas",
  "generic",
] as const;

export type ProviderKind = (typeof providerKinds)[number];
export type ProviderAvailability = "available" | "planned";
export type ProviderCredentialStrategy =
  | "nativeFederation"
  | "dynamicRole"
  | "secureConnector";

export interface ProviderDescriptor {
  id: ProviderKind;
  name: string;
  availability: ProviderAvailability;
  credentialStrategy: ProviderCredentialStrategy;
  supportedEngines: readonly string[];
  leaseSeconds: number | null;
  supportsReadOnly: boolean;
  supportsReadWrite: boolean;
  setupKind: "oauth" | "cloudTrust" | "connector";
  note: string;
}

export const providerCatalog: readonly ProviderDescriptor[] = [
  {
    id: "planetScale",
    name: "PlanetScale",
    availability: "available",
    credentialStrategy: "dynamicRole",
    supportedEngines: ["postgres", "mysql"],
    leaseSeconds: 15 * 60,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "oauth",
    note: "OAuth로 연결하고 구성원별 TTL 역할 또는 비밀번호를 발급합니다.",
  },
  {
    id: "awsRds",
    name: "AWS RDS / Aurora",
    availability: "planned",
    credentialStrategy: "nativeFederation",
    supportedEngines: ["postgres", "mysql"],
    leaseSeconds: 15 * 60,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "cloudTrust",
    note: "교차 계정 IAM 신뢰와 DB의 rds_iam 사용자 설정이 필요합니다.",
  },
  {
    id: "gcpCloudSql",
    name: "GCP Cloud SQL",
    availability: "planned",
    credentialStrategy: "nativeFederation",
    supportedEngines: ["postgres", "mysql"],
    leaseSeconds: 60 * 60,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "cloudTrust",
    note: "Workload Identity Federation과 Cloud SQL IAM 사용자를 사용합니다.",
  },
  {
    id: "oracleOci",
    name: "Oracle OCI Database",
    availability: "planned",
    credentialStrategy: "nativeFederation",
    supportedEngines: ["oracle"],
    leaseSeconds: 60 * 60,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "cloudTrust",
    note: "OCI IAM 토큰, TCPS, Wallet 호환 드라이버가 필요합니다.",
  },
  {
    id: "neon",
    name: "Neon",
    availability: "planned",
    credentialStrategy: "dynamicRole",
    supportedEngines: ["postgres"],
    leaseSeconds: null,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "oauth",
    note: "OAuth 파트너 연동과 안전한 역할 만료 정책이 준비되면 활성화됩니다.",
  },
  {
    id: "mongodbAtlas",
    name: "MongoDB Atlas",
    availability: "planned",
    credentialStrategy: "nativeFederation",
    supportedEngines: ["mongodb"],
    leaseSeconds: null,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "cloudTrust",
    note: "Atlas Workforce OIDC 또는 Workload Identity Federation을 사용합니다.",
  },
  {
    id: "generic",
    name: "Self-hosted / Generic",
    availability: "planned",
    credentialStrategy: "secureConnector",
    supportedEngines: ["postgres", "mysql", "mongodb", "oracle"],
    leaseSeconds: null,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "connector",
    note: "사설망 안의 Secure Connector가 사용자별 자격증명을 발급합니다.",
  },
] as const;

export function isProviderKind(value: string): value is ProviderKind {
  return providerKinds.includes(value as ProviderKind);
}

export function providerDescriptor(provider: string) {
  return providerCatalog.find((item) => item.id === provider) ?? null;
}

