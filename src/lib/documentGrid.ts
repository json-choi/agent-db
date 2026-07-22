// Shapes MongoDB documents into DataGrid columns/rows: union of top-level keys (_id
// first), each cell JSON.stringify'd (undefined -> blank). Shared by the Documents query
// screen and the Tables mongodb branch so both render documents the same way.
function documentColumns(documents: unknown[], fallback: string[] = []): string[] {
  const seen = new Set<string>();
  const cols: string[] = [];
  for (const doc of documents) {
    if (!doc || typeof doc !== "object") continue;
    for (const key of Object.keys(doc as Record<string, unknown>)) {
      if (!seen.has(key)) {
        seen.add(key);
        cols.push(key);
      }
    }
  }
  if (cols.length === 0) return fallback;
  const idIndex = cols.indexOf("_id");
  if (idIndex > 0) {
    cols.splice(idIndex, 1);
    cols.unshift("_id");
  }
  return cols;
}

export function documentsToGrid(
  documents: unknown[],
  fallbackColumns: string[] = [],
): { columns: string[]; rows: string[][] } {
  const columns = documentColumns(documents, fallbackColumns);
  const rows = documents.map((doc) => {
    const record = doc && typeof doc === "object" ? (doc as Record<string, unknown>) : {};
    return columns.map((col) => {
      const value = record[col];
      return value === undefined ? "" : JSON.stringify(value);
    });
  });
  return { columns, rows };
}
