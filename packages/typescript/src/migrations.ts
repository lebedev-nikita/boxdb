import { BoxDB } from "./database.js";
import { BoxDBError } from "./error.js";
import { NativeMigrationSet, runMigrations } from "./native.js";
import { readdir, readFile } from "node:fs/promises";
import { extname, join } from "node:path";

export interface MigrationFile {
  filename: string;
  sql: string;
}
export interface MigrationInfo {
  version: string;
  name: string;
}
export interface MigrationStatus extends MigrationInfo {
  state: "applied" | "pending";
}
export interface MigrationOptions {
  signal?: AbortSignal;
}

/** An immutable, validated set of SQL documents, independent of a connection. */
export class MigrationSet {
  private constructor(private readonly native: NativeMigrationSet) {}

  static async read(directory: string): Promise<MigrationSet> {
    if (typeof directory !== "string") throw new TypeError("migration directory must be a string");
    let files: MigrationFile[];
    try {
      const names = (await readdir(directory)).filter((name) => extname(name) === ".sql");
      files = await Promise.all(
        names.map(async (filename) => ({
          filename,
          sql: new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(
            await readFile(join(directory, filename)),
          ),
        })),
      );
    } catch (cause) {
      throw new BoxDBError(`cannot read migration directory ${directory}`, {
        code: "BOXDB_IO",
        cause,
      });
    }
    return MigrationSet.fromFiles(files);
  }

  static fromFiles(files: readonly MigrationFile[]): MigrationSet {
    if (
      !Array.isArray(files) ||
      files.some(
        (file) => !file || typeof file.filename !== "string" || typeof file.sql !== "string",
      )
    )
      throw new TypeError("migrations must be an array of { filename, sql } documents");
    try {
      return new MigrationSet(new NativeMigrationSet(files));
    } catch (cause) {
      const error = cause as Error & { code?: string };
      throw new BoxDBError(error.message, { cause, code: error.code ?? "BOXDB_INTERNAL" });
    }
  }

  async up(db: BoxDB, options: MigrationOptions = {}): Promise<MigrationInfo[]> {
    if (!(db instanceof BoxDB)) throw new TypeError("migrations require a root BoxDB client");
    return (await db[runMigrations](this.native, "up", options)).migrations!;
  }

  async down(db: BoxDB, options: MigrationOptions = {}): Promise<MigrationInfo | null> {
    if (!(db instanceof BoxDB)) throw new TypeError("migrations require a root BoxDB client");
    return (await db[runMigrations](this.native, "down", options)).migrations![0] ?? null;
  }

  async status(db: BoxDB, options: MigrationOptions = {}): Promise<MigrationStatus[]> {
    if (!(db instanceof BoxDB)) throw new TypeError("migrations require a root BoxDB client");
    return (await db[runMigrations](this.native, "status", options)).statuses!;
  }
}
