// The shared dimOS line icons (dim-icons.js) as a React element; size follows the font, color follows the text.
import { DIM_ICON_PATHS } from "../dim-icons.js"

export type IconName = keyof typeof DIM_ICON_PATHS

export function Icon({ name }: { name: IconName }) {
    return (
        <svg className="dim-icon" viewBox="0 0 24 24" aria-hidden="true">
            <path d={DIM_ICON_PATHS[name]} />
        </svg>
    )
}
