export function Section({ title, children, icon }: { title: string; children: React.ReactNode; icon?: React.ReactNode }) {
  return (
    <section className="set-section">
      <h2>
        {icon}
        {title}
      </h2>
      <div className="set-card">{children}</div>
    </section>
  );
}

export function Row({
  label,
  hint,
  warn,
  icon,
  stack,
  children,
}: {
  label: string;
  hint?: string;
  warn?: boolean;
  icon?: React.ReactNode;
  /** Put the control on its own full-width line (wide controls, long hints). */
  stack?: boolean;
  children?: React.ReactNode;
}) {
  return (
    <div className={`set-row ${stack ? "stack" : ""}`}>
      {icon && <span className="set-icon">{icon}</span>}
      <div className="set-label">
        <span>{label}</span>
        {hint && <span className={`set-hint ${warn ? "warn" : ""}`}>{hint}</span>}
      </div>
      <div className="set-control">{children}</div>
    </div>
  );
}

export type DotState = "off" | "starting" | "online" | "error" | "downloading" | "warn" | "idle";

export function Dot({ state }: { state: DotState }) {
  return <span className={`live-dot ${state}`} />;
}
