import { CircleAlertIcon } from "lucide-react"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import type { SfError } from "@/lib/api"

/** Salesforce errors, each with its code. */
export function SfErrors({ errors }: { errors: SfError[] }) {
  return (
    <Alert variant="destructive">
      <CircleAlertIcon />
      <AlertTitle>
        {errors.length === 1
          ? "Salesforce refused this"
          : `${errors.length} errors`}
      </AlertTitle>
      <AlertDescription>
        <ul className="flex flex-col gap-1">
          {errors.map((e, i) => (
            <li key={i}>
              <code className="font-mono text-xs font-semibold">
                {e.errorCode ?? e.statusCode ?? "ERROR"}
              </code>{" "}
              <span className="whitespace-pre-wrap">{e.message}</span>
            </li>
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  )
}
