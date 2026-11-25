import { CheckIcon, XIcon } from "lucide-react"

import { dateTime, text } from "@/lib/format"
import type { Field } from "@/lib/types"

/** A value shown for reading, formatted for its field's type. */
export function FieldValue({
  field,
  value,
}: {
  field?: Field
  value: unknown
}) {
  if (value == null || value === "") {
    return <span className="text-muted-foreground">—</span>
  }
  if (!field) {
    return <>{text(value)}</>
  }
  switch (field.describeType) {
    case "boolean":
      return value ? (
        <CheckIcon className="size-4" aria-label="Yes" />
      ) : (
        <XIcon className="size-4 text-muted-foreground" aria-label="No" />
      )
    case "currency":
      return (
        <>
          {Number(value).toLocaleString(undefined, {
            minimumFractionDigits: 2,
            maximumFractionDigits: 2,
          })}
        </>
      )
    case "percent":
      return <>{`${Number(value).toLocaleString()}%`}</>
    case "double":
    case "int":
      return <>{Number(value).toLocaleString()}</>
    case "datetime":
      return <>{dateTime(text(value))}</>
    case "multipicklist":
      return <>{text(value).split(";").join(", ")}</>
    default:
      return <>{text(value)}</>
  }
}
