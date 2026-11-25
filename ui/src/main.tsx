import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { createBrowserRouter, redirect } from "react-router"
import { RouterProvider } from "react-router/dom"

import "./index.css"
import "./theme.css"
import { ThemeProvider } from "@/components/theme-provider"
import { reloadOnChangeOr, rootLoader } from "@/lib/root"
import { DataLayout, dataIndexLoader } from "@/routes/data"
import { FieldsView } from "@/routes/fields"
import { LogEntryView, LogView, logEntryLoader } from "@/routes/log"
import { MetadataView, metadataLoader } from "@/routes/metadata"
import { NotFound } from "@/routes/not-found"
import {
  ObjectView,
  objectLoader,
  objectShouldRevalidate,
} from "@/routes/object"
import { QueryView } from "@/routes/query"
import { RecordDrawer, newRecordLoader, recordLoader } from "@/routes/record"
import { RecordsView, recordsLoader } from "@/routes/records"
import { Loading, Root, RouteError } from "@/routes/root"

/** The server serves the app here. */
const BASE = "/__localforce/ui"

// Links from before the app moved to path routes: #/data/Account is now /data/Account.
if (location.hash.startsWith("#/")) {
  history.replaceState(null, "", `${BASE}/${location.hash.slice(2)}`)
}

const router = createBrowserRouter(
  [
    {
      id: "root",
      path: "/",
      loader: rootLoader,
      // The objects' counts change with every edit, not with every navigation.
      shouldRevalidate: reloadOnChangeOr(),
      Component: Root,
      ErrorBoundary: RouteError,
      HydrateFallback: Loading,
      children: [
        { index: true, loader: () => redirect("/data") },
        {
          path: "data",
          Component: DataLayout,
          children: [
            { index: true, loader: dataIndexLoader },
            {
              id: "object",
              path: ":object",
              loader: objectLoader,
              shouldRevalidate: objectShouldRevalidate,
              Component: ObjectView,
              ErrorBoundary: RouteError,
              children: [
                {
                  id: "records",
                  loader: recordsLoader,
                  Component: RecordsView,
                  ErrorBoundary: RouteError,
                  children: [
                    { index: true },
                    {
                      id: "new-record",
                      path: "new",
                      loader: newRecordLoader,
                      Component: RecordDrawer,
                    },
                    {
                      id: "record",
                      path: ":id",
                      loader: recordLoader,
                      Component: RecordDrawer,
                      ErrorBoundary: RouteError,
                    },
                  ],
                },
                { path: "fields", Component: FieldsView },
                {
                  path: "metadata",
                  loader: metadataLoader,
                  Component: MetadataView,
                  ErrorBoundary: RouteError,
                },
              ],
            },
          ],
        },
        { path: "query", Component: QueryView },
        {
          path: "log",
          Component: LogView,
          children: [
            { index: true },
            {
              path: ":id",
              loader: logEntryLoader,
              Component: LogEntryView,
              ErrorBoundary: RouteError,
            },
          ],
        },
        { path: "*", Component: NotFound },
      ],
    },
  ],
  { basename: BASE }
)

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider storageKey="localforce.theme">
      <RouterProvider router={router} />
    </ThemeProvider>
  </StrictMode>
)
