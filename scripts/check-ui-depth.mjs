// Static guard for DopeDB's UI layout contract. It walks JSX rather than raw DOM
// depth: only surfaces and interactive controls consume one of the three levels.
// It also checks that control rows opt in to shared sizing and that fractional grid
// tracks cannot grow past their container.
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { parse } from "@babel/parser";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourceRoots = [path.join(root, "src"), path.join(root, "workspace-cloud", "app")];
const maxVisualDepth = 3;
const controlTags = new Set(["button", "input", "select", "textarea", "summary"]);
const explicitSurfaceClasses = new Set([
  "btn",
  "badge",
  "card",
  "ds-card",
  "ds-panel",
  "ds-surface",
  "editor-box",
  "generated-sql",
  "grid-panel",
  "grid-scroll",
  "safety-details",
  "schema-detail-list",
  "schema-node",
]);
const cssBoundaryClasses = new Set();
const surfaceClassPattern = /(?:^|[-_])(?:card|panel|surface|modal|dialog|popover|inspector|canvas-wrap)$/;
const controlRowClassPattern = /(?:^|[-_])(?:actions|pager|tabs|toolbar)$/;

function filesBelow(directory, extension) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const resolved = path.join(directory, entry.name);
    if (entry.isDirectory()) return filesBelow(resolved, extension);
    return entry.isFile() && resolved.endsWith(extension) ? [resolved] : [];
  });
}

const tsxFiles = sourceRoots.flatMap((sourceRoot) => filesBelow(sourceRoot, ".tsx"));
const cssFiles = sourceRoots.flatMap((sourceRoot) => filesBelow(sourceRoot, ".css"));

function attribute(opening, name) {
  return opening.attributes.find(
    (property) => property.type === "JSXAttribute" && property.name.name === name,
  );
}

function literalStrings(node, values = []) {
  if (!node || typeof node !== "object") return values;
  if (node.type === "StringLiteral") values.push(node.value);
  if (node.type === "TemplateElement") values.push(node.value.raw);
  for (const [key, child] of Object.entries(node)) {
    if (["loc", "start", "end", "extra"].includes(key)) continue;
    if (Array.isArray(child)) {
      for (const item of child) literalStrings(item, values);
    } else {
      literalStrings(child, values);
    }
  }
  return values;
}

function classNames(opening) {
  const className = attribute(opening, "className");
  if (!className?.value) return [];
  return literalStrings(className.value)
    .flatMap((value) => value.split(/\s+/))
    .filter(Boolean);
}

function tagName(opening) {
  const name = opening.name;
  if (name.type === "JSXIdentifier") return name.name;
  if (name.type === "JSXMemberExpression") {
    return `${name.object.name}.${name.property.name}`;
  }
  return "unknown";
}

function isVisualBoundary(opening) {
  if (attribute(opening, "data-ui-boundary")) return true;
  const tag = tagName(opening);
  if (controlTags.has(tag) || /(?:Button|Card|Panel|Dialog|Modal|Inspector)$/.test(tag)) return true;
  return classNames(opening).some(
    (name) =>
      explicitSurfaceClasses.has(name) ||
      cssBoundaryClasses.has(name) ||
      surfaceClassPattern.test(name),
  );
}

function checkControlRow(opening, file, errors) {
  const names = classNames(opening);
  if (
    names.some((name) => controlRowClassPattern.test(name)) &&
    !names.includes("ds-control-row")
  ) {
    errors.push(
      `${path.relative(root, file)}:${opening.loc?.start.line ?? 1} ` +
        `control row must include ds-control-row: ${names.join(" ")}`,
    );
  }
}

function walk(node, file, depth, ancestry, errors) {
  if (!node || typeof node !== "object") return;
  const opening = node.type === "JSXElement" ? node.openingElement : null;
  if (opening) checkControlRow(opening, file, errors);
  const boundary = opening ? isVisualBoundary(opening) : false;
  const nextDepth = depth + Number(boundary);
  const nextAncestry = boundary && opening
    ? [...ancestry, `${tagName(opening)}.${classNames(opening).join(".")}`]
    : ancestry;

  if (opening && nextDepth > maxVisualDepth) {
    errors.push(
      `${path.relative(root, file)}:${opening.loc?.start.line ?? 1} ` +
        `visual depth ${nextDepth}: ${nextAncestry.join(" > ")}`,
    );
    return;
  }

  if (node.type === "JSXElement" || node.type === "JSXFragment") {
    for (const child of node.children) walk(child, file, nextDepth, nextAncestry, errors);
    return;
  }

  for (const [key, child] of Object.entries(node)) {
    if (["loc", "start", "end", "extra", "comments", "errors"].includes(key)) continue;
    if (Array.isArray(child)) {
      for (const item of child) walk(item, file, depth, ancestry, errors);
    } else {
      walk(child, file, depth, ancestry, errors);
    }
  }
}

const guardFixture = parse(
  '<section className="ds-panel"><article className="card"><div className="ds-surface"><button>Too deep</button></div></article></section>',
  { sourceType: "module", plugins: ["jsx"] },
);
const guardFixtureErrors = [];
walk(guardFixture, "<ui-depth-self-test>", 0, [], guardFixtureErrors);
if (guardFixtureErrors.length === 0) {
  throw new Error("UI depth guard self-test failed to detect a four-level boundary");
}

function targetClassNames(selectorList) {
  const names = [];
  for (const selector of selectorList.split(",")) {
    const lastCompound = selector.trim().split(/[\s>+~]+/).at(-1) ?? "";
    const match = lastCompound.match(/\.([A-Za-z_][\w-]*)/);
    if (match) names.push(match[1]);
  }
  return names;
}

function positiveDeclaration(body, property) {
  const match = body.match(new RegExp(`(?:^|;)\\s*${property}\\s*:\\s*([^;]+)`, "i"));
  if (!match) return false;
  return !/^(?:0(?:px)?|none|transparent|inherit|initial|unset)(?:\s|$)/i.test(
    match[1].trim(),
  );
}

for (const file of cssFiles) {
  const source = fs.readFileSync(file, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  const rule = /([^{}]+)\{([^{}]*)\}/g;
  for (const match of source.matchAll(rule)) {
    const body = match[2];
    const border = positiveDeclaration(body, "border");
    const radius = positiveDeclaration(body, "border-radius");
    const background =
      positiveDeclaration(body, "background") ||
      positiveDeclaration(body, "background-color");
    const shadow = positiveDeclaration(body, "box-shadow");
    if (!(shadow || (border && (radius || background)) || (radius && background))) {
      continue;
    }
    for (const name of targetClassNames(match[1])) cssBoundaryClasses.add(name);
  }
}

const errors = [];
for (const file of tsxFiles) {
  const source = fs.readFileSync(file, "utf8");
  const ast = parse(source, {
    sourceType: "module",
    plugins: ["jsx", "typescript"],
  });
  walk(ast, file, 0, [], errors);
}

function containsUnsafeFractionalTrack(value) {
  let minmaxDepth = 0;
  for (let index = 0; index < value.length; index += 1) {
    if (value.startsWith("minmax(", index)) {
      minmaxDepth += 1;
      index += "minmax(".length - 1;
      continue;
    }
    if (value[index] === ")" && minmaxDepth > 0) {
      minmaxDepth -= 1;
      continue;
    }
    if (minmaxDepth === 0) {
      const match = value.slice(index).match(/^(?:\d*\.?\d+)fr\b/);
      if (match) return true;
    }
  }
  return false;
}

for (const file of cssFiles) {
    const source = fs.readFileSync(file, "utf8");
    const sourceWithoutComments = source.replace(/\/\*[\s\S]*?\*\//g, (comment) =>
      comment.replace(/[^\n]/g, " "),
    );
    const declaration = /grid-template-(?:columns|rows)\s*:\s*([^;{}]+);/g;
    for (const match of sourceWithoutComments.matchAll(declaration)) {
      if (!containsUnsafeFractionalTrack(match[1])) continue;
      const line = source.slice(0, match.index).split("\n").length;
      errors.push(
        `${path.relative(root, file)}:${line} unsafe fractional grid track: ${match[1].trim()}`,
      );
    }

    const rule = /([^{}]+)\{([^{}]*)\}/g;
    for (const match of sourceWithoutComments.matchAll(rule)) {
      const selector = match[1];
      const targetsControl =
        /(?:^|[\s>+~,(:])(?:button|input|select|summary)(?=[\s.#[:>+~,)\]]|$)/.test(selector) ||
        /[._-](?:btn|button|seg)(?:[^\w-]|$)/.test(selector);
      if (!targetsControl) continue;
      const hardCodedHeight = match[2].match(
        /(?:^|;)\s*(?:min-)?height\s*:\s*(\d+(?:\.\d+)?px)\b/,
      );
      if (!hardCodedHeight) continue;
      const line = source.slice(0, match.index).split("\n").length;
      errors.push(
        `${path.relative(root, file)}:${line} control height must use a --ds-control-* token: ` +
          `${selector.trim()} (${hardCodedHeight[1]})`,
      );
    }
}

if (errors.length > 0) {
  console.error(`UI layout contract failed (visual depth maximum ${maxVisualDepth}):`);
  for (const error of errors) console.error(`- ${error}`);
  console.error(
    "Use whitespace/dividers, ds-control-row, --ds-control-* tokens, and minmax(0, 1fr).",
  );
  process.exit(1);
}

console.log(
  `UI layout contract passed for ${tsxFiles.length} TSX and ${cssFiles.length} CSS files ` +
    `(visual depth maximum ${maxVisualDepth}).`,
);
