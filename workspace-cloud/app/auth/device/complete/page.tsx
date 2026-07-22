import { Brand } from "../../../components/Brand";

export default async function DeviceCompletePage({
  searchParams,
}: {
  searchParams: Promise<{ denied?: string }>;
}) {
  const denied = Boolean((await searchParams).denied);
  return (
    <main className="single-shell">
      <Brand />
      <section className="device-card complete-card">
        <div className="success-mark">{denied ? "×" : "✓"}</div>
        <p className="eyebrow">{denied ? "DEVICE DENIED" : "DEVICE AUTHORIZED"}</p>
        <h1>{denied ? "요청을 거절했습니다." : "연결되었습니다."}</h1>
        <p>DopeDB 앱으로 돌아가세요. 이 브라우저 창은 닫아도 됩니다.</p>
      </section>
    </main>
  );
}
