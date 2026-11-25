import { PlugIcon } from "lucide-react"
import { useRevalidator } from "react-router"
import { toast } from "sonner"

import { CodeBlock } from "@/components/code-block"
import { ConfirmButton } from "@/components/confirm-button"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import { actingUser, api } from "@/lib/api"
import { useRoot } from "@/lib/root"

/** How to point an app at this server, and resetting the org. */
export function ConnectDialog() {
  const { info } = useRoot()
  const { revalidate } = useRevalidator()
  const user =
    info.users.find((u) => u.id === actingUser(info)) ?? info.users[0]
  const rows: [string, string][] = [
    ["Instance URL", info.httpUrl],
    ["HTTPS", info.httpsUrl],
    ["Certificate", info.certificate],
    ["API version", `${info.apiVersion} (any version works)`],
    ["Org id", info.orgId],
    ["Users", info.users.map((u) => u.username).join(", ")],
    [
      "Storage",
      info.persistent && info.dataDir
        ? info.dataDir
        : "In memory (lost on restart; use --data-dir to keep it)",
    ],
  ]
  if (info.metadata.length) rows.push(["Metadata", info.metadata.join(", ")])

  async function reset() {
    await api("/reset", { method: "POST" })
    toast("All records deleted")
    await revalidate()
  }

  return (
    <Dialog>
      <DialogTrigger render={<Button variant="outline" size="sm" />}>
        <PlugIcon data-icon="inline-start" />
        Connect an app
      </DialogTrigger>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Connect an app</DialogTitle>
          <DialogDescription>
            Point your app's Salesforce client at this server instead of{" "}
            <code>login.salesforce.com</code>. Any client id, secret and
            password is accepted; usernames must belong to a user below.
          </DialogDescription>
        </DialogHeader>
        <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-sm">
          {rows.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted-foreground">{k}</dt>
              <dd className="min-w-0 font-mono text-xs break-all">{v}</dd>
            </div>
          ))}
        </dl>
        <CodeBlock
          title="Get an access token"
          code={`curl -s ${info.httpUrl}/services/oauth2/token \\\n  -d grant_type=password -d client_id=any -d client_secret=any \\\n  -d username=${user.username} -d password=any`}
        />
        <CodeBlock
          title="jsforce (Node.js)"
          code={`const conn = new jsforce.Connection({ loginUrl: "${info.httpUrl}", version: "${info.apiVersion}" });\nawait conn.login("${user.username}", "any password");`}
        />
        <CodeBlock
          title="simple-salesforce (Python, needs HTTPS)"
          code={`import requests\nfrom simple_salesforce import Salesforce\n\ns = requests.Session()\ns.verify = "${info.certificate}"\ntoken = s.post("${info.httpsUrl}/services/oauth2/token", data={"grant_type": "client_credentials"}).json()\nsf = Salesforce(instance_url=token["instance_url"], session_id=token["access_token"], session=s, version="${info.apiVersion}")`}
        />
        <CodeBlock
          title="Environment variables (if your app reads them)"
          code={`SALESFORCE_LOGIN_URL=${info.httpUrl}\nSALESFORCE_INSTANCE_URL=${info.httpUrl}\nSALESFORCE_USERNAME=${user.username}\nSALESFORCE_PASSWORD=any`}
        />
        <div className="flex items-center justify-between gap-4 rounded-lg border border-destructive/30 p-3">
          <div className="text-sm">
            <div className="font-medium">Delete all records</div>
            <div className="text-muted-foreground">
              Every record except users. The schema is kept.
            </div>
          </div>
          <ConfirmButton
            variant="destructive"
            title="Delete all records?"
            description="Every record in this org except users is deleted. The schema is kept."
            action="Delete all records"
            onConfirm={reset}
          >
            Delete all records
          </ConfirmButton>
        </div>
      </DialogContent>
    </Dialog>
  )
}
