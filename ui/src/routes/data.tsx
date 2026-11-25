import { PlusIcon, SearchIcon } from "lucide-react"
import * as React from "react"
import {
  Link,
  Outlet,
  redirect,
  useNavigate,
  useParams,
  useRevalidator,
} from "react-router"
import { toast } from "sonner"

import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button, buttonVariants } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group"
import { Input } from "@/components/ui/input"
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import { api } from "@/lib/api"
import { apiNameFromLabel, pluralLabel } from "@/lib/format"
import { useRoot } from "@/lib/root"
import { stored } from "@/lib/storage"
import type { ObjectSummary } from "@/lib/types"
import { cn } from "@/lib/utils"

/** `/data` opens the object looked at last, or Account. */
export const dataIndexLoader = () =>
  redirect(`/data/${encodeURIComponent(stored("object") ?? "Account")}`)

export function DataLayout() {
  return (
    <ResizablePanelGroup orientation="horizontal">
      <ResizablePanel defaultSize="16%" minSize="12%">
        <ObjectList />
      </ResizablePanel>
      <ResizableHandle />
      <ResizablePanel defaultSize="84%" minSize="50%">
        <Outlet />
      </ResizablePanel>
    </ResizablePanelGroup>
  )
}

function ObjectList() {
  const { objects } = useRoot()
  const { object: current } = useParams()
  const [filter, setFilter] = React.useState("")
  const q = filter.trim().toLowerCase()
  const shown = objects.filter(
    (o) =>
      !q ||
      o.name.toLowerCase().includes(q) ||
      o.label.toLowerCase().includes(q)
  )
  const groups: [string, ObjectSummary[]][] = [
    ["Custom objects", shown.filter((o) => o.custom)],
    ["Standard objects", shown.filter((o) => !o.custom)],
  ]
  return (
    <nav aria-label="Objects" className="flex h-full flex-col gap-2 py-3">
      <div className="flex gap-2 px-3">
        <InputGroup>
          <InputGroupAddon>
            <SearchIcon />
          </InputGroupAddon>
          <InputGroupInput
            type="search"
            placeholder="Filter objects"
            aria-label="Filter objects"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </InputGroup>
        <NewObjectDialog />
      </div>
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-0.5 px-3">
          {groups.map(
            ([title, list]) =>
              list.length > 0 && (
                <React.Fragment key={title}>
                  <div className="px-2.5 pt-3 pb-1 text-xs font-medium text-muted-foreground">
                    {title}
                  </div>
                  {list.map((o) => {
                    const active =
                      o.name.toLowerCase() === current?.toLowerCase()
                    return (
                      <Link
                        key={o.name}
                        to={`/data/${o.name}`}
                        title={o.name}
                        aria-current={active ? "page" : undefined}
                        className={cn(
                          buttonVariants({
                            variant: active ? "secondary" : "ghost",
                            size: "sm",
                          }),
                          "w-full justify-between font-normal"
                        )}
                      >
                        <span className="truncate">{o.labelPlural}</span>
                        <span className="text-xs text-muted-foreground tabular-nums">
                          {o.count.toLocaleString()}
                        </span>
                      </Link>
                    )
                  })}
                </React.Fragment>
              )
          )}
          {!shown.length && (
            <p className="px-2.5 py-2 text-xs text-muted-foreground">
              No objects match.
            </p>
          )}
        </div>
      </ScrollArea>
    </nav>
  )
}

/** Create a custom object: it gets Id, Name, OwnerId, the audit fields and a recycle bin. */
function NewObjectDialog() {
  const navigate = useNavigate()
  const { revalidate } = useRevalidator()
  const [open, setOpen] = React.useState(false)
  const [form, setForm] = React.useState(blankObject)
  const [error, setError] = React.useState<string | null>(null)
  const [busy, setBusy] = React.useState(false)

  // Typing the label fills in the other names until they're edited.
  const setLabel = (label: string) =>
    setForm((f) => {
      const l = label.trim()
      return {
        ...f,
        label,
        name: f.nameTouched ? f.name : l ? apiNameFromLabel(l) : "",
        plural: f.pluralTouched ? f.plural : l ? pluralLabel(l) : "",
        nameLabel: f.nameLabelTouched ? f.nameLabel : l ? `${l} Name` : "",
      }
    })

  async function create() {
    const name = form.name.trim()
    setBusy(true)
    try {
      await api("/objects", {
        method: "POST",
        body: {
          name,
          label: form.label.trim() || null,
          label_plural: form.plural.trim() || null,
          name_field_label: form.nameLabel.trim() || null,
          name_field_format:
            form.nameType === "auto" ? form.format.trim() : null,
          description: form.description.trim() || null,
          fields: [],
        },
      })
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return
    } finally {
      setBusy(false)
    }
    toast(`${name} created`)
    setOpen(false)
    setForm(blankObject)
    await revalidate()
    await navigate(`/data/${name}/fields`)
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next)
        setError(null)
      }}
    >
      <DialogTrigger
        render={
          <Button variant="outline" size="icon" title="New custom object" />
        }
      >
        <PlusIcon />
        <span className="sr-only">New custom object</span>
      </DialogTrigger>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>New custom object</DialogTitle>
          <DialogDescription>
            Like a real custom object, it gets Id, Name, OwnerId, the audit
            fields and a recycle bin. It's stored in this org's database; export
            it as SFDX metadata from its metadata tab.
          </DialogDescription>
        </DialogHeader>
        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        <FieldGroup>
          <Field>
            <FieldLabel htmlFor="object-label">Label</FieldLabel>
            <Input
              id="object-label"
              placeholder="Invoice"
              value={form.label}
              onChange={(e) => setLabel(e.target.value)}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="object-plural">Plural label</FieldLabel>
            <Input
              id="object-plural"
              placeholder="Invoices"
              value={form.plural}
              onChange={(e) =>
                setForm({
                  ...form,
                  plural: e.target.value,
                  pluralTouched: true,
                })
              }
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="object-name">API name</FieldLabel>
            <Input
              id="object-name"
              className="font-mono"
              placeholder="Invoice__c"
              value={form.name}
              onChange={(e) =>
                setForm({ ...form, name: e.target.value, nameTouched: true })
              }
            />
            <FieldDescription>
              Ends in __c. Record ids get a key prefix derived from it.
            </FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="object-name-label">
              Record name label
            </FieldLabel>
            <Input
              id="object-name-label"
              placeholder="Invoice Name"
              value={form.nameLabel}
              onChange={(e) =>
                setForm({
                  ...form,
                  nameLabel: e.target.value,
                  nameLabelTouched: true,
                })
              }
            />
          </Field>
          <Field>
            <FieldLabel>Record name type</FieldLabel>
            <Select
              items={NAME_TYPES}
              value={form.nameType}
              onValueChange={(v) => v && setForm({ ...form, nameType: v })}
            >
              <SelectTrigger className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {NAME_TYPES.map((t) => (
                  <SelectItem key={t.value} value={t.value}>
                    {t.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
          {form.nameType === "auto" && (
            <Field>
              <FieldLabel htmlFor="object-format">Display format</FieldLabel>
              <Input
                id="object-format"
                className="font-mono"
                value={form.format}
                onChange={(e) => setForm({ ...form, format: e.target.value })}
              />
              <FieldDescription>e.g. INV-{"{0000}"}</FieldDescription>
            </Field>
          )}
          <Field>
            <FieldLabel htmlFor="object-description">Description</FieldLabel>
            <Textarea
              id="object-description"
              rows={2}
              value={form.description}
              onChange={(e) =>
                setForm({ ...form, description: e.target.value })
              }
            />
          </Field>
        </FieldGroup>
        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            Cancel
          </Button>
          <Button
            onClick={() => void create()}
            disabled={busy || !form.name.trim()}
          >
            Create object
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

const NAME_TYPES = [
  { label: "Text", value: "text" },
  { label: "Auto Number", value: "auto" },
]

const blankObject = {
  label: "",
  plural: "",
  pluralTouched: false,
  name: "",
  nameTouched: false,
  nameLabel: "",
  nameLabelTouched: false,
  nameType: "text",
  format: "INV-{0000}",
  description: "",
}
