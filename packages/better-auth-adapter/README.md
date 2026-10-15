# BoxDB database adapter for Better Auth

A direct adapter for Better Auth 1.7.x. Install `@boxdb/client`,
`@boxdb/better-auth-adapter`, and `better-auth`.

```sh
npm install @boxdb/client @boxdb/better-auth-adapter better-auth
```

```ts
import { BoxDB } from "@boxdb/client";
import { betterAuth } from "better-auth";
import { betterAuthAdapter } from "@boxdb/better-auth-adapter";

const client = await BoxDB.open("auth.db");
export const auth = betterAuth({
  database: betterAuthAdapter(client),
  secret: process.env.BETTER_AUTH_SECRET,
  baseURL: process.env.BETTER_AUTH_URL,
  emailAndPassword: { enabled: true },
});
// Close client when the application shuts down.
```

The adapter uses Better Auth's factory for model/field mappings, IDs, dates,
booleans, JSON and arrays. Set `usePlural` or `debugLogs` in the adapter's second
argument when needed. Both generated string IDs and serial numeric IDs work.
Better Auth transaction callbacks use explicit BoxDB transactions internally.
Native single-statement `consumeOne` and `incrementOne` preserve atomicity under
concurrent requests.

Use Better Auth's schema generation command, or generate SQL directly:

```ts
const options = { emailAndPassword: { enabled: true } };
const adapter = betterAuthAdapter(client)(options);
const schema = await adapter.createSchema!(options);
await client.exec(schema.code); // Initial schema creation, once.
```

Generation creates an initial schema; it does not diff or migrate existing
schemas. Review generated SQL before applying plugin/schema changes. Better Auth
handles relation fetching through its adapter-factory fallback. Integer numeric
fields must fit JavaScript's safe range; booleans are mapped from bigint storage results.
The adapter borrows the client and never closes it.
