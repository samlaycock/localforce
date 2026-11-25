import { DownloadIcon, FileCodeIcon, InfoIcon } from "lucide-react"
import { useLoaderData, type LoaderFunctionArgs } from "react-router"

import { CodeBlock } from "@/components/code-block"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty"
import { ScrollArea } from "@/components/ui/scroll-area"
import { api } from "@/lib/api"
import { download } from "@/lib/format"

type Metadata = { root: string; files: { path: string; content: string }[] }

export const metadataLoader = ({ params, request }: LoaderFunctionArgs) =>
  api<Metadata>(
    `/objects/${encodeURIComponent(params.object ?? "")}/metadata`,
    { signal: request.signal }
  ).then((m) => ({ object: params.object ?? "", ...m }))

/** The object's custom definitions as SFDX source files. */
export function MetadataView() {
  const { object, root, files } = useLoaderData<typeof metadataLoader>()
  if (!files.length) {
    return (
      <Empty className="h-full">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <FileCodeIcon />
          </EmptyMedia>
          <EmptyTitle>No metadata to export</EmptyTitle>
          <EmptyDescription>{object} has no custom fields.</EmptyDescription>
        </EmptyHeader>
      </Empty>
    )
  }
  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-4 p-6">
        <Alert>
          <InfoIcon />
          <AlertDescription>
            <p>
              SFDX source files for this object's custom definitions. Save them
              under <code>{root}/</code> in your Salesforce project to deploy
              them, or point localforce at the project with{" "}
              <code>--metadata</code>. Definitions in metadata take precedence
              over ones created here.
            </p>
          </AlertDescription>
        </Alert>
        {files.map((f) => (
          <div key={f.path} className="flex flex-col gap-2">
            <div className="flex items-center gap-2">
              <code className="text-sm font-medium">{f.path}</code>
              <Button
                variant="outline"
                size="xs"
                className="ml-auto"
                onClick={() => download(f.path.split("/").pop()!, f.content)}
              >
                <DownloadIcon data-icon="inline-start" />
                Download
              </Button>
            </div>
            <CodeBlock title={f.path.split("/").pop()} code={f.content} />
          </div>
        ))}
      </div>
    </ScrollArea>
  )
}
