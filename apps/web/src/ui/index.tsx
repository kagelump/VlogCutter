import {
  Fragment,
  forwardRef,
  useEffect,
  useId,
  useRef,
  useState,
  type AnchorHTMLAttributes,
  type ButtonHTMLAttributes,
  type ReactElement,
  type ReactNode,
} from "react";
import { Link, NavLink, type LinkProps } from "react-router";
import { ProjectClientError } from "@rcdvlog/project-client";
import { describeError } from "../lib/errors";
import { Icon, type IconName } from "./Icon";
import styles from "./ui.module.css";

export { Icon, type IconName } from "./Icon";

function cx(...names: Array<string | false | null | undefined>) {
  return names.filter(Boolean).join(" ");
}

/* ---------- Buttons ---------- */

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md" | "lg";

interface ButtonLookProps {
  variant?: Variant;
  size?: Size;
  icon?: IconName;
  /** Render only the icon; `label` becomes the accessible name and tooltip. */
  iconOnly?: boolean;
  block?: boolean;
}

function buttonClass({ variant = "secondary", size = "md", iconOnly, block }: ButtonLookProps, extra?: string) {
  return cx(styles.button, styles[variant], size !== "md" && styles[size], iconOnly && styles.iconOnly, block && styles.block, extra);
}

export interface ButtonProps extends ButtonLookProps, ButtonHTMLAttributes<HTMLButtonElement> {
  busy?: boolean;
  label?: string;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant, size, icon, iconOnly, block, busy, label, className, children, type = "button", disabled, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      className={buttonClass({ variant, size, iconOnly, block }, className)}
      disabled={disabled || busy}
      aria-busy={busy || undefined}
      aria-label={iconOnly ? label : rest["aria-label"]}
      title={iconOnly ? label : rest.title}
      {...rest}
    >
      {busy ? <span className={styles.spinner} aria-hidden="true" /> : icon ? <Icon name={icon} size={size === "sm" ? 15 : 18} /> : null}
      {iconOnly ? null : children}
    </button>
  );
});

export function ButtonLink({ variant, size, icon, iconOnly, block, className, children, ...rest }: ButtonLookProps & LinkProps) {
  return (
    <Link className={buttonClass({ variant, size, iconOnly, block }, className)} {...rest}>
      {icon ? <Icon name={icon} size={size === "sm" ? 15 : 18} /> : null}
      {children}
    </Link>
  );
}

export function ButtonAnchor({ variant, size, icon, block, className, children, ...rest }: ButtonLookProps & AnchorHTMLAttributes<HTMLAnchorElement>) {
  return (
    <a className={buttonClass({ variant, size, block }, className)} {...rest}>
      {icon ? <Icon name={icon} size={size === "sm" ? 15 : 18} /> : null}
      {children}
    </a>
  );
}

/* ---------- Status ---------- */

export type Tone = "neutral" | "positive" | "warning" | "critical" | "accent";

export function Badge({ tone = "neutral", children, title }: { tone?: Tone; children: ReactNode; title?: string }) {
  return <span className={styles.badge} data-tone={tone} title={title}>{children}</span>;
}

export function Loading({ label = "Loading…", center = false }: { label?: string; center?: boolean }) {
  return (
    <div className={cx(styles.loading, center && styles.center)} role="status">
      <span className={styles.spinner} aria-hidden="true" />
      {label}
    </div>
  );
}

export function EmptyState({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className={styles.empty}>
      <h2>{title}</h2>
      {children ? <p>{children}</p> : null}
      {action}
    </div>
  );
}

export function Notice({ tone = "neutral", title, children, actions }: { tone?: "neutral" | "warning" | "critical"; title?: ReactNode; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className={styles.notice} data-tone={tone} role={tone === "critical" ? "alert" : "note"}>
      <Icon name="alert" size={16} />
      <div>
        {title ? <strong>{title}</strong> : null}
        {title && children ? " " : null}
        {children}
        {actions ? <div className={styles.noticeActions}>{actions}</div> : null}
      </div>
    </div>
  );
}

/** An inline error with the server request ID when there is one. */
export function ErrorMessage({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  if (!error) return null;
  const requestId = error instanceof ProjectClientError ? error.requestId : null;
  return (
    <div className={styles.notice} data-tone="critical" role="alert">
      <Icon name="alert" size={16} />
      <div>
        {describeError(error)}
        {requestId ? <span className={styles.requestId}>Request {requestId}</span> : null}
        {onRetry ? <div className={styles.noticeActions}><Button size="sm" onClick={onRetry}>Retry</Button></div> : null}
      </div>
    </div>
  );
}

/* ---------- Layout ---------- */

export function PageHeader({ title, subtitle, back, actions, titleExtra }: {
  title: ReactNode;
  subtitle?: ReactNode;
  back?: { to: string; label: string };
  actions?: ReactNode;
  titleExtra?: ReactNode;
}) {
  return (
    <header className={styles.pageHeader}>
      <div className={styles.pageHeaderText}>
        {back ? <Link className={styles.back} to={back.to}><Icon name="chevronLeft" size={16} />{back.label}</Link> : null}
        <h1 className={styles.pageTitle}>{title}{titleExtra}</h1>
        {subtitle ? <p className={styles.pageSubtitle}>{subtitle}</p> : null}
      </div>
      {actions ? <div className={styles.pageActions}>{actions}</div> : null}
    </header>
  );
}

export function Panel({ title, description, actions, children, className, labelledBy }: {
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
  className?: string;
  labelledBy?: string;
}) {
  const id = useId();
  return (
    <section className={cx(styles.panel, className)} aria-labelledby={title ? labelledBy ?? id : undefined}>
      {title || actions ? (
        <div className={styles.panelHeader}>
          <div>
            {title ? <h2 id={labelledBy ?? id} className={styles.panelTitle}>{title}</h2> : null}
            {description ? <p className={styles.panelDescription}>{description}</p> : null}
          </div>
          {actions}
        </div>
      ) : null}
      {children}
    </section>
  );
}

/** A collapsible section. Children mount on first open, so large lists cost nothing until asked for. */
export function Disclosure({ title, meta, children, defaultOpen = false }: {
  title: ReactNode;
  meta?: ReactNode;
  children: ReactNode;
  defaultOpen?: boolean;
}) {
  const [opened, setOpened] = useState(defaultOpen);
  return (
    <details className={styles.disclosure} open={defaultOpen} onToggle={(event) => { if ((event.currentTarget as HTMLDetailsElement).open) setOpened(true); }}>
      <summary><Icon name="chevronDown" size={16} />{title}{meta ? <small>{meta}</small> : null}</summary>
      {opened ? <div className={styles.disclosureBody}>{children}</div> : null}
    </details>
  );
}

export function Facts({ items }: { items: Array<[ReactNode, ReactNode] | null | false> }) {
  return (
    <dl className={styles.facts}>
      {items.filter(Boolean).map((item, index) => {
        const [term, value] = item as [ReactNode, ReactNode];
        return <Fragment key={index}><dt>{term}</dt><dd>{value}</dd></Fragment>;
      })}
    </dl>
  );
}

export function Stats({ items, label }: { items: Array<{ label: string; value: ReactNode; detail?: ReactNode } | null | false>; label?: string }) {
  return (
    <dl className={styles.stats} aria-label={label}>
      {items.filter(Boolean).map((item) => {
        const { label: term, value, detail } = item as { label: string; value: ReactNode; detail?: ReactNode };
        return <div key={term} className={styles.stat}><dt>{term}</dt><dd>{value}{detail ? <small>{detail}</small> : null}</dd></div>;
      })}
    </dl>
  );
}

export function Tabs({ items, label }: { items: Array<{ to: string; label: string; end?: boolean; icon?: IconName }>; label: string }) {
  return (
    <nav className={styles.tabs} aria-label={label}>
      {items.map((item) => (
        <NavLink key={item.to} to={item.to} end={item.end} className={styles.tab}>
          {item.icon ? <Icon name={item.icon} size={16} /> : null}
          {item.label}
        </NavLink>
      ))}
    </nav>
  );
}

export function Segmented<T extends string>({ value, options, onChange, label }: { value: T; options: Array<{ value: T; label: string }>; onChange: (value: T) => void; label: string }) {
  return (
    <div className={styles.segmented} role="group" aria-label={label}>
      {options.map((option) => (
        <button key={option.value} type="button" aria-pressed={value === option.value} onClick={() => onChange(option.value)}>{option.label}</button>
      ))}
    </div>
  );
}

/* ---------- Forms ---------- */

/** Label + control + hint. The control receives the generated `id`. */
export function Field({ label, hint, children, className }: { label: ReactNode; hint?: ReactNode; children: (id: string) => ReactElement; className?: string }) {
  const id = useId();
  const hintId = `${id}-hint`;
  return (
    <div className={cx(styles.field, className)}>
      <label className={styles.fieldLabel} htmlFor={id}>{label}</label>
      {children(id)}
      {hint ? <p id={hintId} className={styles.fieldHint}>{hint}</p> : null}
    </div>
  );
}

export function Checkbox({ checked, onChange, children, disabled }: { checked: boolean; onChange: (checked: boolean) => void; children: ReactNode; disabled?: boolean }) {
  return (
    <label className={styles.check}>
      <input type="checkbox" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
      <span>{children}</span>
    </label>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className={styles.kbd}>{children}</kbd>;
}

export function Mono({ children, title }: { children: ReactNode; title?: string }) {
  return <span className={styles.mono} title={title}>{children}</span>;
}

/* ---------- Dialog ---------- */

/**
 * Native modal dialog: focus trapping, Escape and the top layer come from the
 * browser. Becomes a bottom sheet on narrow screens.
 */
export function Dialog({ open, onClose, title, children, footer, wide }: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  wide?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) {
      if (typeof dialog.showModal === "function") dialog.showModal();
      else dialog.setAttribute("open", "");
    } else if (!open && dialog.open) {
      if (typeof dialog.close === "function") dialog.close();
      else dialog.removeAttribute("open");
    }
  }, [open]);
  if (!open) return null;
  return (
    <dialog
      ref={ref}
      className={cx(styles.dialog, wide && styles.wide)}
      aria-labelledby={titleId}
      onCancel={(event) => { event.preventDefault(); onClose(); }}
      onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}
    >
      <div className={styles.dialogHeader}>
        <h2 id={titleId}>{title}</h2>
        <Button variant="ghost" icon="close" iconOnly label="Close" onClick={onClose} />
      </div>
      <div className={styles.dialogBody}>{children}</div>
      {footer ? <div className={styles.dialogFooter}>{footer}</div> : null}
    </dialog>
  );
}

export function ConfirmDialog({ open, title, children, confirmLabel, onConfirm, onClose, busy, danger = true }: {
  open: boolean;
  title: string;
  children: ReactNode;
  confirmLabel: string;
  onConfirm: () => void;
  onClose: () => void;
  busy?: boolean;
  danger?: boolean;
}) {
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={title}
      footer={<>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant={danger ? "danger" : "primary"} busy={busy} onClick={onConfirm}>{confirmLabel}</Button>
      </>}
    >
      <div>{children}</div>
    </Dialog>
  );
}

export const ui = styles;
