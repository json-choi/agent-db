// Short relative timestamps for feed/audit rows. No deps — app-side Date is fine.
const MON = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

export function relTime(t: string | number): string {
  const d = new Date(t);
  const s = (Date.now() - d.getTime()) / 1000;
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  if (s < 172800) return "yesterday";
  if (s < 604800) return `${Math.round(s / 86400)}d`;
  const abs = `${MON[d.getMonth()]} ${d.getDate()}`;
  return d.getFullYear() === new Date().getFullYear() ? abs : `${abs} ${d.getFullYear()}`;
}

export function fullTime(t: string | number): string {
  return new Date(t).toLocaleString();
}
