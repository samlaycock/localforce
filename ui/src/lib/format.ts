// Formatting and naming helpers.

export const NUMERIC = ["int", "double", "currency", "percent"]

/** Salesforce's UTC datetimes (2026-11-12T08:17:22.000+0000) as `2026-11-12 08:17:22`. */
export const dateTime = (v: string) => `${v.slice(0, 10)} ${v.slice(11, 19)}`

/** A SOQL string literal. */
export const soqlString = (s: string) =>
  `'${s.replace(/\\/g, "\\\\").replace(/'/g, "\\'")}'`

/** A custom API name from a label: "Due Date" → "Due_Date__c". */
export function apiNameFromLabel(label: string) {
  let base = label
    .trim()
    .replace(/[^A-Za-z0-9]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_|_$/g, "")
  if (!/^[A-Za-z]/.test(base)) base = `X${base}`
  return `${base.slice(0, 40).replace(/_$/, "")}__c`
}

/** An English plural for an object label: Invoice → Invoices, Company → Companies. */
export function pluralLabel(label: string) {
  if (label.endsWith("s")) return `${label}es`
  if (label.endsWith("y") && !/[aeiou]y$/i.test(label)) {
    return `${label.slice(0, -1)}ies`
  }
  return `${label}s`
}

/** JSON pretty-printed, or the text as it is. */
export function pretty(text: string) {
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return text
  }
}

/** A request path with its query string readable. */
export function decodePath(path: string) {
  try {
    return decodeURIComponent(path.replace(/\+/g, " "))
  } catch {
    return path
  }
}

export const plural = (n: number, one: string, many = `${one}s`) =>
  `${n.toLocaleString()} ${n === 1 ? one : many}`

/** Save text as a file. */
export function download(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: "text/plain" }))
  const a = document.createElement("a")
  a.href = url
  a.download = name
  document.body.append(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}

/** A field value as text: scalars as they are, objects as JSON, nothing as "". */
export function text(v: unknown): string {
  switch (typeof v) {
    case "string":
      return v
    case "number":
    case "boolean":
    case "bigint":
      return String(v)
    case "undefined":
      return ""
    default:
      return v === null ? "" : JSON.stringify(v)
  }
}
