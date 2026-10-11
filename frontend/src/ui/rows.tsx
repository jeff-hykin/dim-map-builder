// One annotation in a panel's list: its color (yours, the agent's, a no-go area), a rename box, what it is, delete.
import type { ReactNode } from "react"
import { IconButton, ListRow, StatusDot, Text, TextInput } from "dimos-react"

export function AnnotationRow(
    { name, onRename, tone = "ok", toneLabel, detail, selected, onClick, onDelete, extra, ...rest }: {
        name: string
        onRename: (name: string) => void
        tone?: "ok" | "info" | "danger" | "warn"
        toneLabel?: string
        detail?: ReactNode
        selected?: boolean
        onClick?: () => void
        onDelete: () => void
        extra?: ReactNode
    } & Record<string, unknown>,
) {
    return (
        <ListRow
            icon={<StatusDot tone={tone} label={toneLabel} />}
            title={<TextInput size="sm" value={name} onCommit={onRename} onClick={(event: MouseEvent) => event.stopPropagation()} />}
            meta={detail === undefined ? undefined : <Text small mono muted>{detail}</Text>}
            actions={
                <>
                    {extra}
                    <IconButton icon="close" size="sm" label="Delete (undoable)" onClick={onDelete} />
                </>
            }
            selected={selected}
            onClick={onClick}
            {...rest}
        />
    )
}
