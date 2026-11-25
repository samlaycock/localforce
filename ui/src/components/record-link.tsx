import { Link } from "react-router"

import { objectOfId, useRoot } from "@/lib/root"

/** A link to a record, by its id's key prefix, showing its name when known. */
export function RecordLink({
  id,
  label,
}: {
  id: string
  label?: string | null
}) {
  const { objects } = useRoot()
  const object = objectOfId(objects, id)
  if (!object) return <>{label ?? id}</>
  return (
    <Link
      to={`/data/${object.name}/${id}`}
      className="text-primary hover:underline"
      onClick={(e) => e.stopPropagation()}
    >
      {label ?? id}
    </Link>
  )
}
