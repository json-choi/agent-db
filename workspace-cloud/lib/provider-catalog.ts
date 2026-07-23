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
  setupKind: "oauth" | "apiKey" | "cloudTrust" | "connector";
  resourceLevels: readonly [
    { key: string; kind: string; label: string },
    { key: string; kind: string; label: string },
    { key: string; kind: string; label: string },
  ];
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
    resourceLevels: [
      { key: "organization", kind: "organizations", label: "조직" },
      { key: "database", kind: "databases", label: "DB" },
      { key: "branch", kind: "branches", label: "브랜치" },
    ],
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
    resourceLevels: [
      { key: "account", kind: "accounts", label: "계정" },
      { key: "database", kind: "databases", label: "DB" },
      { key: "endpoint", kind: "endpoints", label: "엔드포인트" },
    ],
    note: "교차 계정 IAM 신뢰와 DB의 rds_iam 사용자 설정이 필요합니다.",
  },
  {
    id: "gcpCloudSql",
    name: "GCP Cloud SQL",
    availability: "available",
    credentialStrategy: "nativeFederation",
    supportedEngines: ["postgres", "mysql"],
    leaseSeconds: 15 * 60,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "cloudTrust",
    resourceLevels: [
      { key: "project", kind: "projects", label: "프로젝트" },
      { key: "instance", kind: "instances", label: "인스턴스" },
      { key: "database", kind: "databases", label: "DB" },
    ],
    note: "Vercel OIDC·Workload Identity Federation으로 15분 IAM 로그인을 발급합니다.",
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
    resourceLevels: [
      { key: "tenancy", kind: "tenancies", label: "테넌시" },
      { key: "database", kind: "databases", label: "DB" },
      { key: "service", kind: "services", label: "서비스" },
    ],
    note: "OCI IAM 토큰, TCPS, Wallet 호환 드라이버가 필요합니다.",
  },
  {
    id: "neon",
    name: "Neon",
    availability: "available",
    credentialStrategy: "dynamicRole",
    supportedEngines: ["postgres"],
    leaseSeconds: 15 * 60,
    supportsReadOnly: true,
    supportsReadWrite: true,
    setupKind: "apiKey",
    resourceLevels: [
      { key: "project", kind: "projects", label: "프로젝트" },
      { key: "branch", kind: "branches", label: "브랜치" },
      { key: "database", kind: "databases", label: "DB" },
    ],
    note: "프로젝트 범위 API 키로 15분 제한 역할을 만들고 만료·회수합니다.",
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
    resourceLevels: [
      { key: "project", kind: "projects", label: "프로젝트" },
      { key: "cluster", kind: "clusters", label: "클러스터" },
      { key: "database", kind: "databases", label: "DB" },
    ],
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
    resourceLevels: [
      { key: "connector", kind: "connectors", label: "커넥터" },
      { key: "service", kind: "services", label: "서비스" },
      { key: "database", kind: "databases", label: "DB" },
    ],
    note: "사설망 안의 Secure Connector가 사용자별 자격증명을 발급합니다.",
  },
] as const;

export function isProviderKind(value: string): value is ProviderKind {
  return providerKinds.includes(value as ProviderKind);
}

export function providerDescriptor(provider: string) {
  return providerCatalog.find((item) => item.id === provider) ?? null;
}
