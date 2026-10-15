The database does not expose complete databases as byte buffers.

```compile_fail,E0599
use boxdb::{Database, Format};
let db = Database::memory();
db.export(Format::Binary);
```

```compile_fail,E0599
use boxdb::Database;
Database::from_snapshot(&[]);
```

```compile_fail,E0599
use boxdb::Database;
Database::restore("restored.db", &[]);
```

```compile_fail,E0599
use boxdb::Database;
Database::read_snapshot("data.db");
```
