import { PencilIcon, PlusIcon, Trash2Icon } from "lucide-react"
import * as React from "react"
import { Link, useRevalidator, useRouteLoaderData } from "react-router"
import { toast } from "sonner"

import { ConfirmButton } from "@/components/confirm-button"
import { SourceBadge } from "@/components/source-badge"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  Field as FormField,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Textarea } from "@/components/ui/textarea"
import { api } from "@/lib/api"
import { apiNameFromLabel, text } from "@/lib/format"
import { useRoot } from "@/lib/root"
import { store, stored } from "@/lib/storage"
import type { Field, Schema } from "@/lib/types"
import type { objectLoader } from "@/routes/object"

/** The field types the editor offers, by the name Salesforce's setup uses. */
const TYPES = [
  ["text", "Text"],
  ["textarea", "Text Area"],
  ["longtext", "Long Text Area"],
  ["email", "Email"],
  ["phone", "Phone"],
  ["url", "URL"],
  ["picklist", "Picklist"],
  ["multipicklist", "Picklist (Multi-Select)"],
  ["checkbox", "Checkbox"],
  ["number", "Number"],
  ["currency", "Currency"],
  ["percent", "Percent"],
  ["date", "Date"],
  ["datetime", "Date/Time"],
  ["lookup", "Lookup Relationship"],
  ["masterdetail", "Master-Detail Relationship"],
  ["autonumber", "Auto Number"],
  ["formula", "Formula"],
] as const

type UiType = (typeof TYPES)[number][0]

const FORMULA_TYPES = [
  { label: "Text", value: "string" },
  { label: "Number", value: "double" },
  { label: "Currency", value: "currency" },
  { label: "Percent", value: "percent" },
  { label: "Checkbox", value: "boolean" },
  { label: "Date", value: "date" },
  { label: "Date/Time", value: "datetime" },
]

function uiType(f: Field): UiType {
  if (f.kind === "autonumber") return "autonumber"
  if (f.kind === "formula") return "formula"
  switch (f.describeType) {
    case "string":
      return "text"
    case "textarea":
      return (f.length ?? 0) > 255 ? "longtext" : "textarea"
    case "boolean":
      return "checkbox"
    case "int":
    case "double":
      return "number"
    case "reference":
      return f.masterDetail ? "masterdetail" : "lookup"
    default:
      return f.describeType as UiType
  }
}

function typeLabel(f: Field) {
  const base = TYPES.find(([k]) => k === uiType(f))?.[1] ?? f.describeType
  if (f.kind === "system") return [base, "system"]
  if (f.kind === "compound") return [base, "First + Last name"]
  if (f.describeType === "reference") return [base, `→ ${f.referenceTo}`]
  if (f.kind === "autonumber") return [base, f.autoNumberFormat]
  if (f.kind === "formula") return [base, f.describeType]
  if (["picklist", "multipicklist"].includes(f.describeType)) {
    return [
      base,
      `${f.values.length} values${f.restricted ? ", restricted" : ""}`,
    ]
  }
  if (
    ["string", "textarea", "email", "url", "phone"].includes(f.describeType) &&
    f.length
  ) {
    return [base, `(${f.length})`]
  }
  return [base, null]
}

export function FieldsView() {
  const schema = useRouteLoaderData<typeof objectLoader>("object")!
  const { revalidate } = useRevalidator()
  const [editing, setEditing] = React.useState<Field | "new" | null>(null)

  async function deleteField(f: Field) {
    try {
      await api(`/objects/${schema.name}/fields/${f.name}`, {
        method: "DELETE",
      })
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e))
      return
    }
    store(`cols.${schema.name}`, null)
    toast("Field deleted")
    await revalidate()
  }

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-4 p-6">
        <div className="flex items-center gap-3">
          <p className="text-sm text-muted-foreground">
            {schema.editable
              ? "This object was created here: add, edit and delete its fields."
              : `This object comes from ${schema.sourceName}. Fields you add here are stored in the org.`}
          </p>
          <Button
            size="sm"
            className="ml-auto"
            onClick={() => setEditing("new")}
          >
            <PlusIcon data-icon="inline-start" />
            New field
          </Button>
        </div>
        <div className="rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Label</TableHead>
                <TableHead>API name</TableHead>
                <TableHead>Type</TableHead>
                <TableHead />
                <TableHead>Defined in</TableHead>
                <TableHead />
              </TableRow>
            </TableHeader>
            <TableBody>
              {schema.fields.map((f) => {
                const [type, detail] = typeLabel(f)
                return (
                  <TableRow key={f.name}>
                    <TableCell title={f.description ?? undefined}>
                      {f.label}
                    </TableCell>
                    <TableCell className="font-mono text-xs">
                      {f.name}
                    </TableCell>
                    <TableCell>
                      {type}
                      {detail && (
                        <span className="text-muted-foreground"> {detail}</span>
                      )}
                    </TableCell>
                    <TableCell>
                      <div className="flex gap-1">
                        {f.required && f.kind !== "system" && (
                          <Badge variant="secondary">Required</Badge>
                        )}
                        {f.unique && <Badge variant="secondary">Unique</Badge>}
                        {f.externalId && (
                          <Badge variant="secondary">External ID</Badge>
                        )}
                        {f.nameField && <Badge variant="secondary">Name</Badge>}
                      </div>
                    </TableCell>
                    <TableCell>
                      <SourceBadge source={f.source} />
                    </TableCell>
                    <TableCell className="text-right">
                      {f.editable && (
                        <div className="flex justify-end gap-1">
                          <Button
                            variant="ghost"
                            size="icon-xs"
                            title="Edit"
                            onClick={() => setEditing(f)}
                          >
                            <PencilIcon />
                            <span className="sr-only">Edit {f.label}</span>
                          </Button>
                          <ConfirmButton
                            variant="ghost"
                            size="icon-xs"
                            title={`Delete ${schema.name}.${f.name}?`}
                            description="Its data is deleted too."
                            action="Delete field"
                            onConfirm={() => deleteField(f)}
                          >
                            <Trash2Icon />
                            <span className="sr-only">Delete {f.label}</span>
                          </ConfirmButton>
                        </div>
                      )}
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </div>
        {schema.childRelationships.length > 0 && (
          <>
            <h2 className="mt-2 text-sm font-medium">
              Child relationships (for subqueries)
            </h2>
            <div className="rounded-lg border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Relationship name</TableHead>
                    <TableHead>Child object</TableHead>
                    <TableHead>Field</TableHead>
                    <TableHead>On delete</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {schema.childRelationships.map((c) => (
                    <TableRow key={`${c.childObject}.${c.field}`}>
                      <TableCell className="font-mono text-xs">
                        {c.name}
                      </TableCell>
                      <TableCell>
                        <Link
                          to={`/data/${c.childObject}/fields`}
                          className="text-primary hover:underline"
                        >
                          {c.childObject}
                        </Link>
                      </TableCell>
                      <TableCell className="font-mono text-xs">
                        {c.field}
                      </TableCell>
                      <TableCell>
                        {c.cascadeDelete ? "Cascade" : "Clear or restrict"}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          </>
        )}
      </div>
      {editing && (
        <FieldDialog
          key={editing === "new" ? "new" : editing.name}
          schema={schema}
          existing={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
        />
      )}
    </ScrollArea>
  )
}

/** Which of the form's rows each type shows. */
const ROWS: Record<string, UiType[]> = {
  length: ["text", "longtext"],
  precision: ["number", "currency", "percent"],
  values: ["picklist", "multipicklist"],
  reference: ["lookup", "masterdetail"],
  restrict: ["lookup"],
  autonumber: ["autonumber"],
  formula: ["formula"],
  default: [
    "text",
    "email",
    "phone",
    "url",
    "number",
    "currency",
    "percent",
    "picklist",
  ],
  defaultBool: ["checkbox"],
  defaultFormula: [
    "text",
    "textarea",
    "longtext",
    "email",
    "phone",
    "url",
    "number",
    "currency",
    "percent",
    "date",
    "datetime",
  ],
  unique: ["text", "email", "phone", "url", "number"],
}

const shows = (row: string, t: UiType) => ROWS[row].includes(t)

function FieldDialog({
  schema,
  existing,
  onClose,
}: {
  schema: Schema
  existing: Field | null
  onClose: () => void
}) {
  const { objects } = useRoot()
  const { revalidate } = useRevalidator()
  const editing = existing !== null
  const v = existing
  const [form, setForm] = React.useState({
    type: (v ? uiType(v) : "text") as UiType,
    label: v?.label ?? "",
    name: v?.name ?? "",
    nameTouched: editing,
    description: v?.description ?? "",
    length: String(v?.length ?? 255),
    precision: String(v?.precision ?? 18),
    scale: String(v?.scale ?? 2),
    required: v?.required ?? false,
    unique: v?.unique ?? false,
    externalId: v?.externalId ?? false,
    values: (v?.values ?? []).join("\n"),
    restricted: v ? v.restricted : true,
    defaultText: text(v?.default),
    defaultBool: v?.default === true,
    defaultFormula: v?.defaultFormula ?? "",
    referenceTo: v?.referenceTo ?? "Account",
    childRel:
      v?.childRelationshipName ??
      `${schema.labelPlural.replace(/[^A-Za-z0-9]/g, "")}__r`,
    restrictDelete: v?.restrictDelete ?? false,
    autoFormat: v?.autoNumberFormat ?? "A-{0000}",
    formula: v?.formula ?? "",
    formulaType:
      v?.describeType && v.kind === "formula" ? v.describeType : "string",
  })
  const [error, setError] = React.useState<string | null>(null)
  const [busy, setBusy] = React.useState(false)
  const set = (patch: Partial<typeof form>) =>
    setForm((f) => ({ ...f, ...patch }))
  const t = form.type

  function setType(type: UiType) {
    // Long text areas default to 32,768 characters, text to 255.
    const length = editing
      ? form.length
      : type === "longtext" && form.length === "255"
        ? "32768"
        : type === "text" && form.length === "32768"
          ? "255"
          : form.length
    set({ type, length })
  }

  async function submit() {
    const num = (x: string) => (x === "" ? null : Number(x))
    const defaultFormula = shows("defaultFormula", t)
      ? form.defaultFormula.trim()
      : ""
    let fixedDefault: unknown = null
    if (t === "checkbox") fixedDefault = form.defaultBool
    else if (
      !defaultFormula &&
      shows("default", t) &&
      form.defaultText !== ""
    ) {
      fixedDefault = ["number", "currency", "percent"].includes(t)
        ? Number(form.defaultText)
        : form.defaultText
    }
    const payload = {
      name: form.name.trim(),
      type: {
        text: "string",
        textarea: "textarea",
        longtext: "textarea",
        email: "email",
        phone: "phone",
        url: "url",
        picklist: "picklist",
        multipicklist: "multipicklist",
        checkbox: "boolean",
        number: "double",
        currency: "currency",
        percent: "percent",
        date: "date",
        datetime: "datetime",
        lookup: "reference",
        masterdetail: "reference",
        autonumber: "string",
        formula: form.formulaType,
      }[t],
      label: form.label.trim() || null,
      description: form.description.trim() || null,
      length: shows("length", t)
        ? num(form.length)
        : t === "textarea"
          ? 255
          : null,
      precision: shows("precision", t) ? num(form.precision) : null,
      scale: shows("precision", t) ? num(form.scale) : null,
      required:
        form.required &&
        !["checkbox", "masterdetail", "autonumber", "formula"].includes(t),
      reference_to: shows("reference", t) ? form.referenceTo : null,
      child_relationship_name: shows("reference", t)
        ? form.childRel.trim() || null
        : null,
      master_detail: t === "masterdetail",
      restrict_delete: t === "lookup" && form.restrictDelete,
      values: shows("values", t)
        ? form.values
            .split("\n")
            .map((x) => x.trim())
            .filter(Boolean)
        : [],
      restricted: shows("values", t) && form.restricted,
      default: fixedDefault,
      default_formula: defaultFormula || null,
      external_id: form.externalId && shows("unique", t),
      unique: form.unique && shows("unique", t),
      auto_number_format: t === "autonumber" ? form.autoFormat.trim() : null,
      formula: t === "formula" ? form.formula.trim() || "null" : null,
    }
    if (!payload.name) {
      setError("An API name is required.")
      return
    }
    setBusy(true)
    try {
      await api(
        editing
          ? `/objects/${schema.name}/fields/${existing.name}`
          : `/objects/${schema.name}/fields`,
        { method: editing ? "PUT" : "POST", body: payload }
      )
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return
    } finally {
      setBusy(false)
    }
    toast(editing ? "Field saved" : `Field ${payload.name} created`)
    if (!editing) {
      // Show the new field in the table straight away.
      const cols = stored(`cols.${schema.name}`)
      if (cols) store(`cols.${schema.name}`, `${cols},${payload.name}`)
    }
    onClose()
    await revalidate()
  }

  const typeItems = TYPES.map(([value, label]) => ({ value, label }))
  const objectItems = objects.map((o) => ({
    value: o.name,
    label: `${o.label} (${o.name})`,
  }))
  const check = (
    key:
      | "required"
      | "unique"
      | "externalId"
      | "restricted"
      | "restrictDelete"
      | "defaultBool",
    label: string
  ) => (
    <Label className="font-normal">
      <Checkbox
        checked={form[key]}
        onCheckedChange={(c) => set({ [key]: c })}
      />
      {label}
    </Label>
  )

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {editing
              ? `Edit ${existing.label}`
              : `New field on ${schema.label}`}
          </DialogTitle>
        </DialogHeader>
        {error && (
          <Alert variant="destructive">
            <AlertDescription className="whitespace-pre-wrap">
              {error}
            </AlertDescription>
          </Alert>
        )}
        <FieldGroup>
          <FormField>
            <FieldLabel>Type</FieldLabel>
            <Select
              items={typeItems}
              value={t}
              onValueChange={(x) => x && setType(x)}
              disabled={editing}
            >
              <SelectTrigger className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {typeItems.map((i) => (
                  <SelectItem key={i.value} value={i.value}>
                    {i.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </FormField>
          <FormField>
            <FieldLabel htmlFor="field-label">Label</FieldLabel>
            <Input
              id="field-label"
              value={form.label}
              onChange={(e) =>
                set({
                  label: e.target.value,
                  name: form.nameTouched
                    ? form.name
                    : e.target.value
                      ? apiNameFromLabel(e.target.value)
                      : "",
                })
              }
            />
          </FormField>
          <FormField>
            <FieldLabel htmlFor="field-name">API name</FieldLabel>
            <Input
              id="field-name"
              className="font-mono"
              value={form.name}
              disabled={editing}
              onChange={(e) => set({ name: e.target.value, nameTouched: true })}
            />
            {!editing && (
              <FieldDescription>
                Ends in __c. Can't be changed later.
              </FieldDescription>
            )}
          </FormField>
          {shows("length", t) && (
            <FormField>
              <FieldLabel htmlFor="field-length">Length</FieldLabel>
              <Input
                id="field-length"
                type="number"
                min={1}
                value={form.length}
                onChange={(e) => set({ length: e.target.value })}
              />
            </FormField>
          )}
          {shows("precision", t) && (
            <div className="grid grid-cols-2 gap-4">
              <FormField>
                <FieldLabel htmlFor="field-precision">Precision</FieldLabel>
                <Input
                  id="field-precision"
                  type="number"
                  min={1}
                  max={18}
                  value={form.precision}
                  onChange={(e) => set({ precision: e.target.value })}
                />
                <FieldDescription>Total digits</FieldDescription>
              </FormField>
              <FormField>
                <FieldLabel htmlFor="field-scale">Decimal places</FieldLabel>
                <Input
                  id="field-scale"
                  type="number"
                  min={0}
                  max={17}
                  value={form.scale}
                  onChange={(e) => set({ scale: e.target.value })}
                />
              </FormField>
            </div>
          )}
          {shows("values", t) && (
            <>
              <FormField>
                <FieldLabel htmlFor="field-values">Values</FieldLabel>
                <Textarea
                  id="field-values"
                  rows={5}
                  placeholder="One value per line"
                  value={form.values}
                  onChange={(e) => set({ values: e.target.value })}
                />
                <FieldDescription>
                  One per line. The default is chosen below.
                </FieldDescription>
              </FormField>
              {check(
                "restricted",
                "Restricted: only these values are accepted"
              )}
            </>
          )}
          {shows("reference", t) && (
            <>
              <FormField>
                <FieldLabel>Related to</FieldLabel>
                <Select
                  items={objectItems}
                  value={form.referenceTo}
                  onValueChange={(x) => x && set({ referenceTo: x })}
                  disabled={editing}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {objectItems.map((i) => (
                      <SelectItem key={i.value} value={i.value}>
                        {i.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </FormField>
              <FormField>
                <FieldLabel htmlFor="field-child-rel">
                  Child relationship name
                </FieldLabel>
                <Input
                  id="field-child-rel"
                  className="font-mono"
                  value={form.childRel}
                  onChange={(e) => set({ childRel: e.target.value })}
                />
                <FieldDescription>
                  For subqueries from the parent: (SELECT … FROM{" "}
                  {form.childRel || "Things__r"})
                </FieldDescription>
              </FormField>
            </>
          )}
          {shows("restrict", t) && (
            <FormField>
              {check(
                "restrictDelete",
                "Don't allow the parent to be deleted while this is set"
              )}
              <FieldDescription>
                Otherwise the field is cleared when the parent is deleted.
              </FieldDescription>
            </FormField>
          )}
          {shows("autonumber", t) && (
            <FormField>
              <FieldLabel htmlFor="field-format">Display format</FieldLabel>
              <Input
                id="field-format"
                className="font-mono"
                value={form.autoFormat}
                onChange={(e) => set({ autoFormat: e.target.value })}
              />
              <FieldDescription>
                e.g. INV-{"{0000}"} or {"{YYYY}{MM}-{000}"}
              </FieldDescription>
            </FormField>
          )}
          {shows("formula", t) && (
            <>
              <FormField>
                <FieldLabel htmlFor="field-formula">Formula</FieldLabel>
                <Textarea
                  id="field-formula"
                  rows={3}
                  className="font-mono"
                  value={form.formula}
                  onChange={(e) => set({ formula: e.target.value })}
                />
                <FieldDescription>
                  Evaluated whenever the record is read, so it can be filtered
                  and sorted on (not grouped). Salesforce's formula functions
                  are supported: IF, CASE, ISBLANK, TEXT, LEFT, CONTAINS, REGEX,
                  ROUND, TODAY, ADDMONTHS, VLOOKUP and the rest.
                </FieldDescription>
              </FormField>
              <FormField>
                <FieldLabel>Result type</FieldLabel>
                <Select
                  items={FORMULA_TYPES}
                  value={form.formulaType}
                  onValueChange={(x) => x && set({ formulaType: x })}
                  disabled={editing}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {FORMULA_TYPES.map((i) => (
                      <SelectItem key={i.value} value={i.value}>
                        {i.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </FormField>
            </>
          )}
          {shows("default", t) && (
            <FormField>
              <FieldLabel htmlFor="field-default">Default value</FieldLabel>
              <Input
                id="field-default"
                value={form.defaultText}
                onChange={(e) => set({ defaultText: e.target.value })}
              />
              <FieldDescription>
                Text, number or picklist value
              </FieldDescription>
            </FormField>
          )}
          {shows("defaultBool", t) &&
            check("defaultBool", "Checked by default")}
          {shows("defaultFormula", t) && (
            <FormField>
              <FieldLabel htmlFor="field-default-formula">
                Default formula
              </FieldLabel>
              <Input
                id="field-default-formula"
                className="font-mono"
                placeholder="e.g. TODAY() + 30"
                value={form.defaultFormula}
                onChange={(e) => set({ defaultFormula: e.target.value })}
              />
              <FieldDescription>
                Instead of a fixed default: a formula worked out when a record
                is created, such as TODAY() + 30 or $User.Email. It can't use
                the record's own fields.
              </FieldDescription>
            </FormField>
          )}
          <div className="flex flex-wrap gap-x-6 gap-y-2">
            {!["checkbox", "masterdetail", "autonumber", "formula"].includes(
              t
            ) && check("required", "Required")}
            {shows("unique", t) && check("unique", "Unique")}
            {shows("unique", t) && check("externalId", "External ID")}
          </div>
          <FormField>
            <FieldLabel htmlFor="field-description">Description</FieldLabel>
            <Textarea
              id="field-description"
              rows={2}
              value={form.description}
              onChange={(e) => set({ description: e.target.value })}
            />
          </FormField>
        </FieldGroup>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={() => void submit()} disabled={busy}>
            {editing ? "Save" : "Create field"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
