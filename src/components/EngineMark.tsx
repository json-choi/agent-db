import mysqlIcon from "../assets/db-icons/mysql.svg";
import mongodbIcon from "../assets/db-icons/mongodb.svg";
import postgresqlIcon from "../assets/db-icons/postgresql.svg";
import sqliteIcon from "../assets/db-icons/sqlite.svg";
import type { Engine } from "../ipc/types";

const ENGINE_ICON: Record<Engine, string> = {
  postgres: postgresqlIcon,
  mysql: mysqlIcon,
  sqlite: sqliteIcon,
  mongodb: mongodbIcon,
};

const ENGINE_LABEL: Record<Engine, string> = {
  postgres: "PostgreSQL",
  mysql: "MySQL",
  sqlite: "SQLite",
  mongodb: "MongoDB",
};

export default function EngineMark({ engine }: { engine: Engine }) {
  const label = ENGINE_LABEL[engine];
  return (
    <span className={`ds-engine-mark engine-${engine}`} title={label} aria-label={label}>
      <img src={ENGINE_ICON[engine]} alt="" aria-hidden="true" draggable={false} />
    </span>
  );
}
