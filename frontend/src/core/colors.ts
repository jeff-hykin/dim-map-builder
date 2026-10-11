// The annotation colors, drawn into the 3D view and onto the 2D plan (data, not UI chrome: the same in every skin, so
// a box looks the same in both views and in the agent's screenshots).
export const ANNOTATION_COLORS = {
    /** what you added */
    user: "#7af0a8",
    /** what the agent added */
    agent: "#7fc8f8",
    /** the selection, the slicer's crop, the straighten band */
    selected: "#ffd166",
    /** what a cleanup would remove, no-go areas, erasing */
    removal: "#ff5f6d",
    /** the x-ray view's voxels */
    xray: "#5fd0ff",
}
