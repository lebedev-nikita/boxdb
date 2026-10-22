import { customType } from "drizzle-orm/sqlite-core";

/** Exact decimal text; arithmetic and precision validation run in BoxDB. */
export const decimal = customType<{
  data: string;
  driverData: string;
  config: { precision?: number; scale?: number };
}>({
  dataType(config) {
    if (config?.precision === undefined) {
      if (config?.scale !== undefined) throw new RangeError("DECIMAL scale requires precision");
      return "DECIMAL";
    }
    const { precision, scale = 0 } = config;
    if (
      !Number.isInteger(precision) ||
      precision < 1 ||
      precision > 1000 ||
      !Number.isInteger(scale) ||
      scale < 0 ||
      scale > precision
    )
      throw new RangeError("Invalid DECIMAL precision or scale");
    return `DECIMAL(${precision},${scale})`;
  },
});

/** Canonical strings preserve BoxDB's full native temporal precision. */
export const uuid = customType<{ data: string; driverData: string }>({ dataType: () => "UUID" });
export const instant = customType<{ data: string; driverData: string }>({
  dataType: () => "INSTANT",
});
export const date = customType<{ data: string; driverData: string }>({ dataType: () => "DATE" });
export const time = customType<{ data: string; driverData: string }>({ dataType: () => "TIME" });
export const dateTime = customType<{ data: string; driverData: string }>({
  dataType: () => "DATETIME",
});
export const interval = customType<{ data: string; driverData: string }>({
  dataType: () => "INTERVAL",
});

/** Serialize complete documents in the client; BoxDB validates the JSON column. */
export const json = customType<{ data: unknown; driverData: string }>({
  dataType: () => "JSON",
  toDriver(value) {
    const text = JSON.stringify(value);
    if (text === undefined) throw new TypeError("Value is not a JSON document");
    return text;
  },
  fromDriver: (value) => JSON.parse(value) as unknown,
});
