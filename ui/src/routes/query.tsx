import { ChevronRightIcon, PlayIcon } from "lucide-react"
import * as React from "react"
import { useLocation } from "react-router"

import { CodeBlock } from "@/components/code-block"
import { RecordLink } from "@/components/record-link"
import { SfErrors } from "@/components/sf-errors"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty"
import { Kbd, KbdGroup } from "@/components/ui/kbd"
import { Label } from "@/components/ui/label"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Spinner } from "@/components/ui/spinner"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Textarea } from "@/components/ui/textarea"
import { api, ApiError, type SfError } from "@/lib/api"
import { text } from "@/lib/format"
import { objectOfId, useRoot } from "@/lib/root"
import { store, stored } from "@/lib/storage"
import type { QueryResult } from "@/lib/types"

const EXAMPLE =
  "SELECT Id, Name, Industry, Owner.Name, (SELECT Name, Email FROM Contacts)\nFROM Account\nORDER BY Name\nLIMIT 100"
const MOD = /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘" : "Ctrl"

type Run = { result: QueryResult } | { errors: SfError[] }

// The last run outlives the page, so the result is still there after looking at a record.
let lastRun: Run | null = null

function history(): string[] {
  try {
    const list: unknown = JSON.parse(stored("soql.history") ?? "[]")
    return Array.isArray(list) ? list.filter((x) => typeof x === "string") : []
  } catch {
    return []
  }
}

/** The SOQL console: the same engine as `/query`, every row up to 50,000. */
export function QueryView() {
  const { state } = useLocation()
  const [text, setText] = React.useState(
    () => (state as { soql?: string } | null)?.soql ?? stored("soql") ?? EXAMPLE
  )
  const [all, setAll] = React.useState(false)
  const [json, setJson] = React.useState(false)
  const [run, setRun] = React.useState<Run | null>(() => lastRun)
  const [running, setRunning] = React.useState(false)
  const [past, setPast] = React.useState(history)

  async function execute() {
    const soql = text.trim()
    if (!soql) return
    store("soql", soql)
    const next = [soql, ...past.filter((x) => x !== soql)].slice(0, 25)
    store("soql.history", JSON.stringify(next))
    setPast(next)
    setRunning(true)
    let outcome: Run
    try {
      outcome = {
        result: await api<QueryResult>("/query", {
          method: "POST",
          body: { soql, all },
        }),
      }
    } catch (e) {
      const data = e instanceof ApiError ? e.data : null
      outcome = {
        errors: Array.isArray(data)
          ? (data as SfError[])
          : [{ message: e instanceof Error ? e.message : String(e) }],
      }
    }
    lastRun = outcome
    setRun(outcome)
    setRunning(false)
  }

  const historyItems = past.map((h) => ({
    value: h,
    label: h.replace(/\s+/g, " ").slice(0, 90),
  }))

  return (
    <div className="flex h-full flex-col">
      <title>SOQL · localforce</title>
      <div className="flex flex-col gap-3 border-b p-4">
        <Textarea
          className="min-h-36 font-mono text-sm"
          spellCheck={false}
          aria-label="SOQL query"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
              e.preventDefault()
              void execute()
            }
          }}
        />
        <div className="flex flex-wrap items-center gap-4">
          <Button onClick={() => void execute()} disabled={running}>
            {running ? (
              <Spinner data-icon="inline-start" />
            ) : (
              <PlayIcon data-icon="inline-start" />
            )}
            Run
            <KbdGroup>
              <Kbd>{MOD}</Kbd>
              <Kbd>↵</Kbd>
            </KbdGroup>
          </Button>
          <Label className="font-normal">
            <Checkbox checked={all} onCheckedChange={(c) => setAll(c)} />
            Include deleted (queryAll)
          </Label>
          <Label className="font-normal">
            <Checkbox checked={json} onCheckedChange={(c) => setJson(c)} />
            Raw JSON
          </Label>
          {historyItems.length > 0 && (
            <Select
              items={historyItems}
              value={null}
              onValueChange={(v) => v && setText(v)}
            >
              <SelectTrigger size="sm" className="w-48">
                <SelectValue placeholder="History" />
              </SelectTrigger>
              <SelectContent>
                {historyItems.map((h) => (
                  <SelectItem
                    key={h.value}
                    value={h.value}
                    className="font-mono text-xs"
                  >
                    {h.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          {run && "result" in run && (
            <span className="ml-auto text-sm text-muted-foreground tabular-nums">
              {run.result.totalSize.toLocaleString()}{" "}
              {run.result.totalSize === 1 ? "row" : "rows"} · {run.result.ms} ms
              {run.result.truncated && " · showing the first 50,000"}
            </span>
          )}
        </div>
      </div>
      <div className="min-h-0 flex-1">
        <Results run={run} json={json} />
      </div>
    </div>
  )
}

function Results({ run, json }: { run: Run | null; json: boolean }) {
  if (!run) {
    return (
      <Empty className="h-full">
        <EmptyHeader>
          <EmptyTitle>Run a query to see results</EmptyTitle>
          <EmptyDescription>
            Queries use the same engine as{" "}
            <code>/services/data/v62.0/query</code>.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    )
  }
  if ("errors" in run) {
    return (
      <div className="p-4">
        <SfErrors errors={run.errors} />
      </div>
    )
  }
  const r = run.result
  if (json) {
    return (
      <ScrollArea className="h-full">
        <div className="p-4">
          <CodeBlock
            code={JSON.stringify(
              { totalSize: r.totalSize, done: r.done, records: r.records },
              null,
              2
            )}
          />
        </div>
      </ScrollArea>
    )
  }
  if (!r.records.length) {
    return (
      <Empty className="h-full">
        <EmptyHeader>
          <EmptyTitle>
            {r.totalSize ? `COUNT() = ${r.totalSize}` : "No rows"}
          </EmptyTitle>
        </EmptyHeader>
      </Empty>
    )
  }
  return (
    <ScrollArea className="h-full">
      <ResultTable records={r.records} />
    </ScrollArea>
  )
}

type Children = { totalSize: number; records: Record<string, unknown>[] }
type Cell = unknown

const isChildren = (v: unknown): v is Children =>
  v !== null && typeof v === "object" && "records" in v && "totalSize" in v

/** A record with parents flattened into dotted columns; child subqueries stay nested. */
function flatten(
  rec: Record<string, unknown>,
  prefix = "",
  out: Record<string, Cell> = {}
) {
  for (const [k, v] of Object.entries(rec)) {
    if (k === "attributes") continue
    const key = prefix + k
    if (v && typeof v === "object" && !Array.isArray(v) && !isChildren(v)) {
      flatten(v as Record<string, unknown>, `${key}.`, out)
    } else {
      out[key] = v
    }
  }
  return out
}

function ResultTable({ records }: { records: Record<string, unknown>[] }) {
  const rows = records.map((r) => flatten(r))
  const cols: string[] = []
  for (const r of rows)
    for (const k of Object.keys(r)) if (!cols.includes(k)) cols.push(k)
  const [open, setOpen] = React.useState<ReadonlySet<string>>(new Set())
  const toggle = (key: string) =>
    setOpen((current) => {
      const next = new Set(current)
      if (!next.delete(key)) next.add(key)
      return next
    })
  return (
    <Table>
      <TableHeader>
        <TableRow>
          {cols.map((c) => (
            <TableHead key={c} className="font-mono text-xs">
              {c}
            </TableHead>
          ))}
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((row, i) => {
          const expanded = cols.filter(
            (c) => open.has(`${i}.${c}`) && isChildren(row[c])
          )
          return (
            <React.Fragment key={i}>
              <TableRow>
                {cols.map((c) => (
                  <TableCell
                    key={c}
                    className={
                      typeof row[c] === "number"
                        ? "text-right tabular-nums"
                        : "max-w-80 truncate"
                    }
                    title={typeof row[c] === "string" ? row[c] : undefined}
                  >
                    <Value
                      value={row[c]}
                      present={c in row}
                      open={open.has(`${i}.${c}`)}
                      onToggle={() => toggle(`${i}.${c}`)}
                    />
                  </TableCell>
                ))}
              </TableRow>
              {expanded.map((c) => (
                <TableRow key={c} className="hover:bg-transparent">
                  <TableCell colSpan={cols.length} className="bg-muted/30 p-3">
                    <div className="mb-1 font-mono text-xs text-muted-foreground">
                      {c}
                    </div>
                    <div className="rounded-lg border bg-background">
                      <ResultTable records={(row[c] as Children).records} />
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </React.Fragment>
          )
        })}
      </TableBody>
    </Table>
  )
}

function Value({
  value,
  present,
  open,
  onToggle,
}: {
  value: unknown
  present: boolean
  open: boolean
  onToggle: () => void
}) {
  const { objects } = useRoot()
  if (isChildren(value)) {
    return (
      <Button variant="ghost" size="xs" onClick={onToggle}>
        {value.totalSize} {value.totalSize === 1 ? "record" : "records"}
        <ChevronRightIcon
          data-icon="inline-end"
          className={open ? "rotate-90" : ""}
        />
      </Button>
    )
  }
  if (value == null) {
    return (
      <span className="text-muted-foreground">{present ? "null" : ""}</span>
    )
  }
  if (
    typeof value === "string" &&
    /^[a-zA-Z0-9]{18}$/.test(value) &&
    objectOfId(objects, value)
  ) {
    return <RecordLink id={value} />
  }
  return <>{text(value)}</>
}
