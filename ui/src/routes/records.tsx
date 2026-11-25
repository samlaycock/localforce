import {
  ArrowDownIcon,
  ArrowUpIcon,
  Columns3Icon,
  DatabaseIcon,
  PlusIcon,
  SearchIcon,
  XIcon,
} from "lucide-react"
import * as React from "react"
import {
  Link,
  Outlet,
  useLoaderData,
  useLocation,
  useMatches,
  useNavigate,
  useParams,
  useRevalidator,
  useSearchParams,
  type LoaderFunctionArgs,
} from "react-router"

import { FieldValue } from "@/components/field-value"
import { RecordLink } from "@/components/record-link"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button, buttonVariants } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import {
  Empty,
  EmptyContent,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group"
import { Label } from "@/components/ui/label"
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { api, ApiError } from "@/lib/api"
import { NUMERIC, text } from "@/lib/format"
import {
  columns,
  fieldOf,
  loadSchema,
  parentName,
  parentNamePath,
  selectPaths,
} from "@/lib/schema"
import { store } from "@/lib/storage"
import type { RecordsPage, Schema, SfRecord } from "@/lib/types"
import { useUrlInput } from "@/lib/url-input"
import { cn } from "@/lib/utils"

const PAGE = 50
const RESET_OFFSET = ["offset"]

/**
 * One page of records. The list's state is in its URL: `q` (search), `deleted=1` (the recycle
 * bin), `order` and `dir`, `offset`, and `field`, `value` and `label` for a related list.
 */
export async function recordsLoader({ params, request }: LoaderFunctionArgs) {
  const schema = await loadSchema(params.object ?? "")
  const url = new URL(request.url).searchParams
  const cols = columns(schema)
  const query = new URLSearchParams({
    fields: selectPaths(schema, cols).join(","),
    limit: String(PAGE),
    offset: url.get("offset") ?? "0",
    order: url.get("order") ?? "",
    dir: url.get("dir") ?? "asc",
    q: url.get("q") ?? "",
    deleted: url.get("deleted") === "1" ? "1" : "0",
  })
  const field = url.get("field")
  const value = url.get("value")
  if (field && value) {
    query.set("field", field)
    query.set("value", value)
  }
  try {
    const page = await api<RecordsPage>(
      `/records/${encodeURIComponent(schema.name)}?${query}`,
      { signal: request.signal }
    )
    return { schema, columns: cols, page, error: null }
  } catch (e) {
    if (e instanceof ApiError) {
      return { schema, columns: cols, page: null, error: e.message }
    }
    throw e
  }
}

export function RecordsView() {
  const {
    schema,
    columns: cols,
    page,
    error,
  } = useLoaderData<typeof recordsLoader>()
  const drawerOpen = useMatches().some(
    (m) => m.id === "record" || m.id === "new-record"
  )
  const table = error ? (
    <Alert variant="destructive" className="m-4 w-auto">
      <AlertDescription className="font-mono text-xs whitespace-pre-wrap">
        {error}
      </AlertDescription>
    </Alert>
  ) : (
    <RecordsTable schema={schema} columns={cols} page={page!} />
  )
  return (
    <div className="flex h-full flex-col">
      <Toolbar schema={schema} />
      <div className="min-h-0 flex-1 border-t">
        {drawerOpen ? (
          <ResizablePanelGroup orientation="horizontal">
            <ResizablePanel defaultSize="58%" minSize="30%">
              {table}
            </ResizablePanel>
            <ResizableHandle />
            <ResizablePanel defaultSize="42%" minSize="28%">
              <Outlet />
            </ResizablePanel>
          </ResizablePanelGroup>
        ) : (
          table
        )}
      </div>
      {page && <Pager page={page} />}
    </div>
  )
}

function Toolbar({ schema }: { schema: Schema }) {
  const [params, setParams] = useSearchParams()
  const { search } = useLocation()
  const [query, setQuery] = useUrlInput("q", RESET_OFFSET)
  const filter = params.get("field") && params.get("value")
  const deleted = params.get("deleted") === "1"
  return (
    <div className="flex flex-wrap items-center gap-3 px-6 py-3">
      <InputGroup className="w-72">
        <InputGroupAddon>
          <SearchIcon />
        </InputGroupAddon>
        <InputGroupInput
          type="search"
          placeholder="Search text fields or paste an Id"
          aria-label="Search records"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </InputGroup>
      {filter && (
        <Badge variant="secondary" className="h-6">
          {params.get("field")} = {params.get("label") ?? params.get("value")}
          <Link to={`/data/${schema.name}`} title="Show all records">
            <XIcon />
          </Link>
        </Badge>
      )}
      <Label className="font-normal">
        <Checkbox
          checked={deleted}
          onCheckedChange={(checked) =>
            setParams((current) => {
              const next = new URLSearchParams(current)
              if (checked) next.set("deleted", "1")
              else next.delete("deleted")
              next.delete("offset")
              return next
            })
          }
        />
        Recycle bin ({schema.deletedCount.toLocaleString()})
      </Label>
      <div className="ml-auto flex items-center gap-2">
        <ColumnsDialog schema={schema} />
        <Link
          to={{ pathname: `/data/${schema.name}/new`, search }}
          className={buttonVariants({ size: "sm" })}
        >
          <PlusIcon data-icon="inline-start" />
          New {schema.label}
        </Link>
      </div>
    </div>
  )
}

function RecordsTable({
  schema,
  columns: cols,
  page,
}: {
  schema: Schema
  columns: string[]
  page: RecordsPage
}) {
  const [params, setParams] = useSearchParams()
  const { search } = useLocation()
  const navigate = useNavigate()
  const { id: selected } = useParams()
  const order = params.get("order")
  const dir = params.get("dir") === "desc" ? "desc" : "asc"

  const sort = (path: string) =>
    setParams((current) => {
      const next = new URLSearchParams(current)
      next.set("dir", order === path && dir === "asc" ? "desc" : "asc")
      next.set("order", path)
      next.delete("offset")
      return next
    })

  if (!page.records.length) {
    const searching = Boolean(params.get("q") || params.get("field"))
    const deleted = params.get("deleted") === "1"
    return (
      <Empty className="h-full">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <DatabaseIcon />
          </EmptyMedia>
          <EmptyTitle>
            {searching
              ? "No records match"
              : deleted
                ? "The recycle bin is empty"
                : `No ${schema.labelPlural.toLowerCase()} yet`}
          </EmptyTitle>
        </EmptyHeader>
        {!searching && !deleted && (
          <EmptyContent>
            <Link
              to={{ pathname: `/data/${schema.name}/new`, search }}
              className={buttonVariants({ size: "sm" })}
            >
              New {schema.label}
            </Link>
          </EmptyContent>
        )}
      </Empty>
    )
  }

  return (
    <ScrollArea className="h-full">
      <Table>
        <TableHeader>
          <TableRow>
            {cols.map((c) => {
              const f = fieldOf(schema, c)
              const path = parentNamePath(f) ?? c
              const active = order === path
              return (
                <TableHead key={c} title={c} className="px-0">
                  <Button
                    variant="ghost"
                    size="sm"
                    className="font-medium"
                    onClick={() => sort(path)}
                  >
                    {f?.label ?? c}
                    {active &&
                      (dir === "asc" ? (
                        <ArrowUpIcon data-icon="inline-end" />
                      ) : (
                        <ArrowDownIcon data-icon="inline-end" />
                      ))}
                  </Button>
                </TableHead>
              )
            })}
          </TableRow>
        </TableHeader>
        <TableBody>
          {page.records.map((r) => (
            <TableRow
              key={r.Id}
              data-state={r.Id === selected ? "selected" : undefined}
              className="cursor-pointer"
              onClick={() =>
                void navigate({
                  pathname: `/data/${schema.name}/${r.Id}`,
                  search,
                })
              }
            >
              {cols.map((c) => (
                <TableCell
                  key={c}
                  className={cn(
                    "max-w-72 truncate",
                    NUMERIC.includes(fieldOf(schema, c)?.describeType ?? "") &&
                      "text-right tabular-nums"
                  )}
                >
                  <Cell schema={schema} record={r} column={c} />
                </TableCell>
              ))}
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </ScrollArea>
  )
}

function Cell({
  schema,
  record,
  column,
}: {
  schema: Schema
  record: SfRecord
  column: string
}) {
  const { search } = useLocation()
  const f = fieldOf(schema, column)
  const value = record[column]
  if (f?.describeType === "reference" && typeof value === "string") {
    return <RecordLink id={value} label={parentName(record, f)} />
  }
  if (f?.nameField) {
    return (
      <Link
        to={{ pathname: `/data/${schema.name}/${record.Id}`, search }}
        className="font-medium text-primary hover:underline"
      >
        {text(value) || record.Id}
      </Link>
    )
  }
  return <FieldValue field={f} value={value} />
}

function Pager({ page }: { page: RecordsPage }) {
  const [params, setParams] = useSearchParams()
  const navigate = useNavigate()
  const offset = Number(params.get("offset") ?? 0)
  const total = page.totalSize
  const from = total ? offset + 1 : 0
  const to = Math.min(offset + PAGE, total)
  const go = (next: number) =>
    setParams((current) => {
      const p = new URLSearchParams(current)
      if (next > 0) p.set("offset", String(next))
      else p.delete("offset")
      return p
    })
  return (
    <div className="flex items-center gap-2 border-t px-6 py-2 text-sm">
      <span className="text-muted-foreground tabular-nums">
        {from.toLocaleString()}–{to.toLocaleString()} of{" "}
        {total.toLocaleString()}
      </span>
      <Button
        variant="outline"
        size="xs"
        disabled={offset === 0}
        onClick={() => go(Math.max(0, offset - PAGE))}
      >
        Previous
      </Button>
      <Button
        variant="outline"
        size="xs"
        disabled={to >= total}
        onClick={() => go(offset + PAGE)}
      >
        Next
      </Button>
      <Button
        variant="link"
        size="xs"
        className="ml-auto"
        title={page.soql}
        onClick={() =>
          void navigate("/query", {
            state: {
              soql: page.soql.replace(/ LIMIT \d+ OFFSET \d+$/, " LIMIT 200"),
            },
          })
        }
      >
        Open in SOQL
      </Button>
    </div>
  )
}

/** Choose the table's columns, kept per object in this browser. */
function ColumnsDialog({ schema }: { schema: Schema }) {
  const { revalidate } = useRevalidator()
  const [open, setOpen] = React.useState(false)
  const [chosen, setChosen] = React.useState<string[]>([])

  async function save(cols: string[] | null) {
    store(`cols.${schema.name}`, cols?.length ? cols.join(",") : null)
    setOpen(false)
    await revalidate()
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (next) setChosen(columns(schema))
        setOpen(next)
      }}
    >
      <DialogTrigger render={<Button variant="outline" size="sm" />}>
        <Columns3Icon data-icon="inline-start" />
        Columns
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Columns</DialogTitle>
        </DialogHeader>
        <ScrollArea className="max-h-[60vh]">
          <div className="flex flex-col gap-2 pr-3">
            {schema.fields.map((f) => (
              <Label key={f.name} className="font-normal">
                <Checkbox
                  checked={chosen.includes(f.name)}
                  onCheckedChange={(checked) =>
                    setChosen((c) =>
                      // Columns that stay keep their place; new ones go last.
                      checked ? [...c, f.name] : c.filter((x) => x !== f.name)
                    )
                  }
                />
                {f.label}
                <code className="text-xs text-muted-foreground">{f.name}</code>
              </Label>
            ))}
          </div>
        </ScrollArea>
        <DialogFooter>
          <Button variant="outline" onClick={() => void save(null)}>
            Reset to default
          </Button>
          <Button onClick={() => void save(chosen)}>Apply</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
