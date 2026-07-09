import { Icon } from "./Icon";

export default function InfoTip({
  label,
  className,
}: {
  label: string;
  className?: string;
}) {
  return (
    <span
      className={"ui-help" + (className ? ` ${className}` : "")}
      title={label}
      aria-label={label}
      role="img"
    >
      <Icon name="info" />
    </span>
  );
}
