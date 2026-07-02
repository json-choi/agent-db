// Client-side statement splitter — mirrors the Rust migrations::split_statements +
// is_effective_sql so the SQL screen can decide single-vs-script mode, badge the count,
// and list statements in the approval panel to match what the backend will execute.
// Purely a UX heuristic: the backend re-splits authoritatively before running.
//
// Runnable check (no test runner in this project — same convention as sqlBuild.ts):
//   npx esbuild src/lib/sqlStatements.ts --bundle --format=esm --outfile=/tmp/ss.mjs \
//     && node --input-type=module -e "import('/tmp/ss.mjs').then(m=>{m.__selfTest();console.log('ok')})"

function stripComments(s: string): string {
  // Good enough for the "is this only comments?" check: a real statement keeps its
  // leading keyword even if this over-strips a "--"/"/*" that sits inside a string.
  return s.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/--[^\n]*/g, " ");
}

// A dollar-quote opener at position i ($$ or $tag$), else null.
function dollarTag(sql: string, i: number): string | null {
  const m = /^\$[A-Za-z_]?[A-Za-z0-9_]*\$/.exec(sql.slice(i));
  return m ? m[0] : null;
}

function push(out: string[], raw: string): void {
  const s = raw.trim();
  if (s && stripComments(s).trim()) out.push(s); // drop empty + comment-only segments
}

// Split into top-level statements, respecting single/double quotes, dollar-quoted
// strings, and line/block comments. Comment-only and empty segments are dropped.
export function splitStatements(sql: string): string[] {
  const out: string[] = [];
  const n = sql.length;
  let start = 0;
  let i = 0;
  while (i < n) {
    const c = sql[i];
    if (c === "'" || c === '"') {
      i++;
      while (i < n) {
        if (sql[i] === c) {
          if (sql[i + 1] === c) {
            i += 2; // doubled-quote escape
            continue;
          }
          i++;
          break;
        }
        i++;
      }
    } else if (c === "-" && sql[i + 1] === "-") {
      while (i < n && sql[i] !== "\n") i++;
    } else if (c === "/" && sql[i + 1] === "*") {
      i += 2;
      while (i + 1 < n && !(sql[i] === "*" && sql[i + 1] === "/")) i++;
      i = Math.min(i + 2, n);
    } else if (c === "$") {
      const tag = dollarTag(sql, i);
      if (tag) {
        i += tag.length;
        const end = sql.indexOf(tag, i);
        i = end < 0 ? n : end + tag.length;
      } else {
        i++;
      }
    } else if (c === ";") {
      push(out, sql.slice(start, i));
      i++;
      start = i;
    } else {
      i++;
    }
  }
  push(out, sql.slice(start));
  return out;
}

export function __selfTest(): void {
  const eq = (got: number, want: number, msg: string) => {
    if (got !== want) throw new Error(`selfTest: ${msg} — got ${got}, want ${want}`);
  };
  eq(splitStatements("SELECT 1;").length, 1, "single");
  eq(splitStatements("SELECT 1; SELECT 2;").length, 2, "two");
  eq(splitStatements("SELECT 1").length, 1, "no trailing semicolon");
  eq(splitStatements("").length, 0, "empty");
  eq(splitStatements("  \n -- just a comment").length, 0, "comment-only");
  eq(splitStatements("INSERT INTO t VALUES ('a;b');").length, 1, "semicolon in string");
  eq(splitStatements("/* a; b */ SELECT 1;").length, 1, "semicolon in block comment");
  eq(splitStatements("SELECT '' AS x; SELECT 2;").length, 2, "empty string literal");
  eq(splitStatements("DO $$ BEGIN; END; $$; SELECT 1;").length, 2, "dollar-quoted block");
}
