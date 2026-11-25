import * as React from "react"
import { Link } from "react-router"

import { Checkbox } from "@/components/ui/checkbox"
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
} from "@/components/ui/combobox"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import { api } from "@/lib/api"
import { NUMERIC, soqlString, text } from "@/lib/format"
import { useRoot } from "@/lib/root"
import type { Field, QueryResult } from "@/lib/types"

/**
 * What a control holds: a checkbox's boolean, or the text its input shows ("" for none). The
 * text stays as typed, so a half-typed number such as "1." isn't rewritten.
 */
export type InputValue = string | boolean

/** A field's API value as its control shows it. */
export function toInput(f: Field, v: unknown): InputValue {
  if (f.describeType === "boolean") return Boolean(v)
  if (v == null) return ""
  // The datetime input has no time zone: show the UTC time Salesforce returns.
  if (f.describeType === "datetime") return text(v).slice(0, 19)
  return text(v)
}

/** A control's value as the REST API takes it. */
export function fromInput(f: Field, v: InputValue): unknown {
  if (typeof v === "boolean") return v
  if (v === "") return null
  if (NUMERIC.includes(f.describeType)) return Number(v)
  if (f.describeType === "datetime") {
    return `${v.length === 16 ? `${v}:00` : v}Z`
  }
  return v
}

/** The input for one field, matched to its type. */
export function FieldControl({
  field: f,
  id,
  value,
  onChange,
  invalid,
  parentLabel,
}: {
  field: Field
  id: string
  value: InputValue
  onChange: (value: InputValue) => void
  invalid: boolean
  /** A lookup's current parent name. */
  parentLabel?: string | null
}) {
  const text = typeof value === "string" ? value : ""
  switch (f.describeType) {
    case "boolean":
      return (
        <Checkbox
          id={id}
          checked={value === true}
          onCheckedChange={(checked) => onChange(checked)}
          aria-invalid={invalid || undefined}
        />
      )
    case "picklist": {
      // A value that's no longer in the list still shows.
      const values =
        text && !f.values.includes(text) ? [...f.values, text] : f.values
      const items = [
        { label: "--None--", value: "" },
        ...values.map((v) => ({ label: v, value: v })),
      ]
      return (
        <Select
          items={items}
          value={text}
          onValueChange={(v) => onChange(v ?? "")}
        >
          <SelectTrigger
            id={id}
            className="w-full"
            aria-invalid={invalid || undefined}
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {items.map((i) => (
              <SelectItem key={i.value} value={i.value}>
                {i.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )
    }
    case "multipicklist": {
      const chosen = new Set(text ? text.split(";") : [])
      return (
        <div id={id} className="flex flex-wrap gap-x-4 gap-y-2">
          {f.values.map((v) => (
            <Label key={v} className="font-normal">
              <Checkbox
                checked={chosen.has(v)}
                onCheckedChange={(checked) => {
                  const next = f.values.filter((x) =>
                    x === v ? checked : chosen.has(x)
                  )
                  onChange(next.join(";"))
                }}
              />
              {v}
            </Label>
          ))}
        </div>
      )
    }
    case "textarea":
      return (
        <Textarea
          id={id}
          rows={3}
          value={text}
          onChange={(e) => onChange(e.target.value)}
          aria-invalid={invalid || undefined}
        />
      )
    case "date":
      return (
        <Input
          id={id}
          type="date"
          value={text}
          onChange={(e) => onChange(e.target.value)}
          aria-invalid={invalid || undefined}
        />
      )
    case "datetime":
      return (
        <div className="flex items-center gap-2">
          <Input
            id={id}
            type="datetime-local"
            step={1}
            value={text}
            onChange={(e) => onChange(e.target.value)}
            aria-invalid={invalid || undefined}
          />
          <span className="text-xs text-muted-foreground">UTC</span>
        </div>
      )
    case "reference":
      return (
        <Lookup
          field={f}
          id={id}
          value={text}
          label={parentLabel ?? null}
          onChange={onChange}
          invalid={invalid}
        />
      )
  }
  if (NUMERIC.includes(f.describeType)) {
    return (
      <Input
        id={id}
        type="number"
        step={f.describeType === "int" ? 1 : "any"}
        value={text}
        onChange={(e) => onChange(e.target.value)}
        aria-invalid={invalid || undefined}
      />
    )
  }
  const type =
    { email: "email", phone: "tel", url: "url" }[f.describeType] ?? "text"
  return (
    <Input
      id={id}
      type={type}
      value={text}
      maxLength={f.length ?? undefined}
      onChange={(e) => onChange(e.target.value)}
      aria-invalid={invalid || undefined}
    />
  )
}

type Option = { id: string; label: string }

/** A lookup: type to search the parent object by name, or paste an id. */
function Lookup({
  field: f,
  id,
  value,
  label,
  onChange,
  invalid,
}: {
  field: Field
  id: string
  value: string
  label: string | null
  onChange: (value: string) => void
  invalid: boolean
}) {
  const { objects } = useRoot()
  const target = objects.find((o) => o.name === f.referenceTo)
  const nameField = target?.nameField ?? null
  const [selected, setSelected] = React.useState<Option | null>(
    value ? { id: value, label: label ?? value } : null
  )
  const [query, setQuery] = React.useState("")
  const [items, setItems] = React.useState<Option[]>([])

  React.useEffect(() => {
    const q = query.trim()
    if (!q || !f.referenceTo || q === selected?.label) return
    const conditions = []
    if (/^[a-zA-Z0-9]{15}([a-zA-Z0-9]{3})?$/.test(q)) {
      conditions.push(`Id = ${soqlString(q)}`)
    }
    if (nameField) {
      conditions.push(
        `${nameField} LIKE ${soqlString(`%${q.replace(/[%_]/g, "\\$&")}%`)}`
      )
    }
    if (!conditions.length) return
    const soql = `SELECT Id${nameField ? `, ${nameField}` : ""} FROM ${f.referenceTo} WHERE ${conditions.join(" OR ")}${nameField ? ` ORDER BY ${nameField}` : ""} LIMIT 8`
    const controller = new AbortController()
    const timer = setTimeout(() => {
      api<QueryResult>("/query", {
        method: "POST",
        body: { soql },
        signal: controller.signal,
      })
        .then((res) =>
          setItems(
            res.records.map((r) => {
              const rid = String(r.Id)
              const name = nameField ? r[nameField] : null
              return { id: rid, label: typeof name === "string" ? name : rid }
            })
          )
        )
        .catch(() => setItems([]))
    }, 180)
    return () => {
      clearTimeout(timer)
      controller.abort()
    }
  }, [query, f.referenceTo, nameField, selected])

  return (
    <div className="flex items-center gap-2">
      <Combobox
        items={items}
        filter={null}
        value={selected}
        onValueChange={(option: Option | null) => {
          setSelected(option)
          onChange(option?.id ?? "")
        }}
        onInputValueChange={(v) => setQuery(v)}
        itemToStringLabel={(o: Option) => o.label}
        isItemEqualToValue={(a: Option, b: Option) => a.id === b.id}
      >
        <ComboboxInput
          id={id}
          className="w-full"
          placeholder={`Search ${target?.labelPlural ?? f.referenceTo}…`}
          showClear
          aria-invalid={invalid || undefined}
        />
        <ComboboxContent>
          <ComboboxEmpty>No matches</ComboboxEmpty>
          <ComboboxList>
            {(o: Option) => (
              <ComboboxItem key={o.id} value={o}>
                <span className="truncate">{o.label}</span>
                <span className="ml-auto font-mono text-xs text-muted-foreground">
                  {o.id}
                </span>
              </ComboboxItem>
            )}
          </ComboboxList>
        </ComboboxContent>
      </Combobox>
      {selected && f.referenceTo && (
        <Link
          to={`/data/${f.referenceTo}/${selected.id}`}
          className="shrink-0 text-xs text-primary hover:underline"
        >
          Open
        </Link>
      )}
    </div>
  )
}
