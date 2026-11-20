// Inlines app.css into a single self-contained page (embedded in flowscale.exe). No dependencies.
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
mkdirSync("dist", { recursive: true });
const css = readFileSync("src/app.css", "utf8").replace(/\/\*[\s\S]*?\*\//g, "").replace(/\s*\n\s*/g, "\n").replace(/\s*([{};:,>])\s*/g, "$1").trim();
const html = readFileSync("src/index.html", "utf8").replace('<style id="css"></style>', () => `<style>${css}</style>`);
writeFileSync("dist/index.html", html);
console.log(`ui/dist/index.html: ${(html.length / 1024).toFixed(1)} KB`);
