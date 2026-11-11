import type { Labels } from "@aqora/pisa-client";

/** `a, b` as `["a", "b"]`, leaving out blanks. */
export function parseList(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s !== "");
}

/** `a=1, b=2` as labels. */
export function parseLabels(text: string): Labels {
  const labels: Labels = {};
  for (const part of parseList(text)) {
    const [key = "", ...rest] = part.split("=");
    if (key.trim() !== "") labels[key.trim()] = rest.join("=").trim();
  }
  return labels;
}

/** Labels as `a=1, b=2`. */
export function formatLabels(labels: Labels): string {
  return Object.entries(labels)
    .map(([k, v]) => `${k}=${v}`)
    .join(", ");
}
