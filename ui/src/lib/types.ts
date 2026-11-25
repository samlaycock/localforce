// The shapes the UI API returns.

export type Source = "standard" | "config" | "metadata" | "ui"

export type User = { id: string; username: string; name: string | null }

export type Info = {
  version: string
  apiVersion: string
  orgId: string
  httpUrl: string
  httpsUrl: string
  certificate: string
  persistent: boolean
  dataDir: string | null
  metadata: string[]
  defaultUser: string
  users: User[]
}

export type ObjectSummary = {
  name: string
  label: string
  labelPlural: string
  custom: boolean
  keyPrefix: string
  source: Source
  nameField: string | null
  count: number
  deletedCount: number
}

export type Field = {
  name: string
  label: string
  describeType: string
  /** "system", "compound", "autonumber", "formula", or "" for a plain field. */
  kind: string
  formula: string | null
  autoNumberFormat: string | null
  description: string | null
  length: number | null
  precision: number | null
  scale: number | null
  required: boolean
  createable: boolean
  updateable: boolean
  custom: boolean
  source: Source
  /** Created in the UI, so it can be edited or deleted here. */
  editable: boolean
  nameField: boolean
  referenceTo: string | null
  referenceNameField: string | null
  relationshipName: string | null
  childRelationshipName: string | null
  masterDetail: boolean
  restrictDelete: boolean
  values: string[]
  restricted: boolean
  default: unknown
  defaultFormula: string | null
  externalId: boolean
  unique: boolean
}

export type ChildRelationship = {
  name: string
  childObject: string
  field: string
  cascadeDelete: boolean
}

export type Schema = ObjectSummary & {
  sourceName: string
  editable: boolean
  description: string | null
  fields: Field[]
  childRelationships: ChildRelationship[]
}

/** A record as SOQL returns it: field values, with parent records nested by relationship name. */
export type SfRecord = { Id: string } & Record<string, unknown>

export type RecordsPage = {
  totalSize: number
  records: SfRecord[]
  soql: string
}

export type Related = {
  relationship: string
  childObject: string
  childLabelPlural: string
  field: string
  count: number
}

export type QueryResult = {
  totalSize: number
  done: boolean
  records: Record<string, unknown>[]
  truncated: boolean
  ms: number
}

export type LogSummary = {
  id: number
  time: string
  method: string
  path: string
  status: number
  durationMs: number
  ui: boolean
  errorCode: string | null
}

export type LogEntry = {
  id: number
  method: string
  path: string
  status: number
  duration_ms: number
  request_headers: [string, string][]
  request_body: string | null
  response_body: string | null
}
