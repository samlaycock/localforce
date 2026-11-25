import { ActivityIcon, XIcon } from "lucide-react"
import * as React from "react"
import {
  Link,
  Outlet,
  useLoaderData,
  useNavigate,
  useParams,
  type LoaderFunctionArgs,
} from "react-router"

import { CodeBlock } from "@/components/code-block"
import { CopyButton } from "@/components/copy-button"
import { Badge } from "@/components/ui/badge"
import { Button, buttonVariants } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty"
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
import { decodePath, pretty } from "@/lib/format"
import type { LogEntry, LogSummary } from "@/lib/types"

/** How many requests the page keeps, like the server. */
const KEEP = 500

/** Follows the log, polling for the requests after the newest one it has. */
function useLog() {
  const [entries, setEntries] = React.useState<LogSummary[]>([])
  React.useEffect(() => {
    let latest = 0
    let stopped = false
    let busy = false
    async function poll() {
      if (busy) return
      busy = true
      try {
        const fresh = await api<LogSummary[]>(`/requests?after=${latest}`)
        if (stopped || !fresh.length) return
        latest = fresh[fresh.length - 1].id
        setEntries((current) => [...current, ...fresh].slice(-KEEP))
      } catch {
        // The server is restarting; try again on the next tick.
      } finally {
        busy = false
      }
    }
    void poll()
    const timer = setInterval(() => void poll(), 1500)
    return () => {
      stopped = true
      clearInterval(timer)
    }
  }, [])
  return [entries, setEntries] as const
}

const statusVariant = (status: number) =>
  status >= 400 ? "destructive" : status >= 300 ? "outline" : "secondary"

export function LogView() {
  const [entries, setEntries] = useLog()
  const [showUi, setShowUi] = React.useState(false)
  const { id } = useParams()
  const navigate = useNavigate()
  const shown = entries.filter((e) => showUi || !e.ui).toReversed()

  async function clear() {
    await api("/requests", { method: "DELETE" })
    setEntries([])
    await navigate("/log")
  }

  const list = (
    <div className="flex h-full flex-col">
      <div className="flex flex-wrap items-center gap-3 border-b px-4 py-3">
        <h1 className="text-sm font-medium">Requests to the Salesforce APIs</h1>
        <span className="text-xs text-muted-foreground">
          newest first · last {KEEP}
        </span>
        <Label className="ml-auto font-normal">
          <Checkbox checked={showUi} onCheckedChange={(c) => setShowUi(c)} />
          Include this UI's requests
        </Label>
        <Button variant="outline" size="sm" onClick={() => void clear()}>
          Clear
        </Button>
      </div>
      {shown.length ? (
        <ScrollArea className="min-h-0 flex-1">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time</TableHead>
                <TableHead>Method</TableHead>
                <TableHead>Path</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Error</TableHead>
                <TableHead className="text-right">ms</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {shown.map((e) => (
                <TableRow
                  key={e.id}
                  className="cursor-pointer"
                  data-state={String(e.id) === id ? "selected" : undefined}
                  onClick={() => void navigate(`/log/${e.id}`)}
                >
                  <TableCell className="font-mono text-xs">
                    {e.time.slice(11, 23)}
                  </TableCell>
                  <TableCell className="font-mono text-xs">
                    {e.method}
                  </TableCell>
                  <TableCell
                    className="max-w-md truncate font-mono text-xs"
                    title={decodePath(e.path)}
                  >
                    {decodePath(e.path)}
                  </TableCell>
                  <TableCell>
                    <Badge
                      variant={statusVariant(e.status)}
                      className="font-mono"
                    >
                      {e.status}
                    </Badge>
                  </TableCell>
                  <TableCell className="font-mono text-xs">
                    {e.errorCode}
                  </TableCell>
                  <TableCell className="text-right tabular-nums">
                    {e.durationMs}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </ScrollArea>
      ) : (
        <Empty className="flex-1">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <ActivityIcon />
            </EmptyMedia>
            <EmptyTitle>No requests yet</EmptyTitle>
            <EmptyDescription>
              Point an app at this server (see “Connect an app”) and its API
              calls appear here, with the exact reason anything failed.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
    </div>
  )

  return (
    <>
      <title>API log · localforce</title>
      {id ? (
        <ResizablePanelGroup orientation="horizontal">
          <ResizablePanel defaultSize="55%" minSize="30%">
            {list}
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel defaultSize="45%" minSize="25%">
            <Outlet />
          </ResizablePanel>
        </ResizablePanelGroup>
      ) : (
        list
      )}
    </>
  )
}

export async function logEntryLoader({ params }: LoaderFunctionArgs) {
  try {
    return await api<LogEntry>(`/requests/${params.id}`)
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null
    throw e
  }
}

/** One request with its headers and bodies, and the same call as curl. */
export function LogEntryView() {
  const e = useLoaderData<typeof logEntryLoader>()
  const close = (
    <Link
      to="/log"
      className={buttonVariants({ variant: "ghost", size: "icon-sm" })}
      title="Close"
    >
      <XIcon />
      <span className="sr-only">Close</span>
    </Link>
  )
  if (!e) {
    return (
      <Empty className="h-full">
        <EmptyHeader>
          <EmptyTitle>This request is no longer in the log</EmptyTitle>
        </EmptyHeader>
        {close}
      </Empty>
    )
  }
  const curl = [
    `curl -X ${e.method} '${location.origin}${e.path}'`,
    ...e.request_headers
      .filter(([k]) => ["content-type", "sforce-query-options"].includes(k))
      .map(([k, v]) => `-H '${k}: ${v}'`),
    "-H 'authorization: Bearer $TOKEN'",
  ]
  if (e.request_body)
    curl.push(`--data '${e.request_body.replace(/'/g, "'\\''")}'`)
  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-4 p-4">
        <div className="flex items-center gap-2">
          <code className="text-sm font-semibold">{e.method}</code>
          <Badge variant={statusVariant(e.status)} className="font-mono">
            {e.status}
          </Badge>
          <span className="text-sm text-muted-foreground">
            {e.duration_ms} ms
          </span>
          <CopyButton
            className="ml-auto"
            text={curl.join(" \\\n  ")}
            label="Copy as curl"
          />
          {close}
        </div>
        <code className="text-xs break-all">{decodePath(e.path)}</code>
        <div className="flex flex-col gap-1">
          <h3 className="text-xs font-medium text-muted-foreground">
            Request headers
          </h3>
          <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-0.5 font-mono text-xs">
            {e.request_headers.map(([k, v], i) => (
              <div key={i} className="contents">
                <dt className="text-muted-foreground">{k}</dt>
                <dd className="min-w-0 break-all">{v}</dd>
              </div>
            ))}
          </dl>
        </div>
        {e.request_body && (
          <CodeBlock title="Request body" code={pretty(e.request_body)} />
        )}
        {e.response_body ? (
          <CodeBlock title="Response body" code={pretty(e.response_body)} />
        ) : (
          <p className="text-sm text-muted-foreground">
            The response had no body.
          </p>
        )}
      </div>
    </ScrollArea>
  )
}
