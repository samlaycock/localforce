import { CloudIcon } from "lucide-react"
import * as React from "react"
import { Link, matchPath, useLocation } from "react-router"
import { toast } from "sonner"

import { ConnectDialog } from "@/components/connect-dialog"
import { ModeToggle } from "@/components/mode-toggle"
import { Badge } from "@/components/ui/badge"
import { buttonVariants } from "@/components/ui/button"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { actingUser, setActingUser } from "@/lib/api"
import { useRoot } from "@/lib/root"
import { store } from "@/lib/storage"

/** Each nav entry, and the paths that show it as current. */
const NAV = [
  { to: "/data", label: "Data", paths: ["/data/*"] },
  { to: "/query", label: "SOQL", paths: ["/query"] },
  { to: "/log", label: "API log", paths: ["/log/*"] },
]

export function AppHeader() {
  const { info } = useRoot()
  const { pathname } = useLocation()
  return (
    <header className="flex h-12 shrink-0 items-center gap-4 border-b px-4">
      <Link to="/" className="flex items-center gap-2 text-sm font-semibold">
        <span className="flex size-6 items-center justify-center rounded-md bg-primary text-primary-foreground">
          <CloudIcon className="size-3.5" />
        </span>
        localforce
        <Badge variant="secondary" className="font-mono font-normal">
          {info.version}
        </Badge>
      </Link>
      <nav className="flex items-center gap-1">
        {NAV.map(({ to, label, paths }) => {
          const current = paths.some((p) => matchPath(p, pathname))
          return (
            <Link
              key={to}
              to={to}
              aria-current={current ? "page" : undefined}
              className={buttonVariants({
                variant: current ? "secondary" : "ghost",
                size: "sm",
              })}
            >
              {label}
            </Link>
          )
        })}
      </nav>
      <div className="ml-auto flex items-center gap-2">
        <ActingAs />
        <ConnectDialog />
        <ModeToggle />
      </div>
    </header>
  )
}

/** The user records are created and edited as. */
function ActingAs() {
  const { info } = useRoot()
  const [user, setUser] = React.useState(() => actingUser(info))
  const items = info.users.map((u) => ({
    label: u.name ?? u.username,
    value: u.id,
  }))
  return (
    <label className="flex items-center gap-2 text-xs text-muted-foreground">
      Acting as
      <Select
        items={items}
        value={user}
        onValueChange={(id) => {
          if (!id) return
          setUser(id)
          setActingUser(id)
          store("user", id)
          toast(`Now acting as ${items.find((i) => i.value === id)?.label}`)
        }}
      >
        <SelectTrigger size="sm" className="w-40">
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
    </label>
  )
}
