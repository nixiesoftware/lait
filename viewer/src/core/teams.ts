import type { BoardView, ProjectDto, TeamDto } from "../types";

/**
 * A team is a set of projects, so every team surface is a project surface with
 * more than one project in it.
 *
 * That is the whole idea and it is why this scope was cheap to add: the Projects
 * page and the roadmap already take a list of projects, so a team view of them
 * is a filter. Only the issue surfaces needed anything new, because a board is
 * one project's rows and a team has several — hence `mergeBoards` below.
 */

/**
 * The projects a team owns.
 *
 * `ProjectDto.team` is the authority and `TeamDto.projects` is a back-reference
 * the projection maintains; this reads the authority. The two are written
 * together so they should agree, but if they ever drift, the field on the thing
 * being grouped is the one that decides which group it is in.
 *
 * Archived projects are the caller's business — pass the list you want grouped.
 */
export function projectsOf(team: TeamDto, projects: readonly ProjectDto[]): ProjectDto[] {
  return projects.filter((project) => project.team === team.id);
}

/** Projects no team owns. Always rendered, even when empty is the whole story:
 *  a space with no teams is entirely this bucket. */
export function ungrouped(teams: readonly TeamDto[], projects: readonly ProjectDto[]): ProjectDto[] {
  const owned = new Set(teams.map((team) => team.id));
  return projects.filter((project) => !project.team || !owned.has(project.team));
}

/**
 * One board out of several, for a scope that spans projects.
 *
 * The workflow belongs to the space rather than to any project, so every board
 * in a space carries the same columns in the same order — which is what makes
 * this a concatenation rather than a merge with a conflict rule. Columns are
 * keyed by state id and taken from the first board that has them, so a project
 * still loading contributes nothing rather than dropping a column everyone else
 * has.
 *
 * `project` is synthesised from the team, because `BoardView` has a project on
 * it and every consumer reads its `name` for a heading or its `id` for a
 * resource scope. A heading saying the team's name is the honest answer for a
 * board that is several projects wide; a scope id that is the team's is
 * likewise the right grain for anything caching this.
 *
 * Returns `null` when nothing has loaded, which callers already handle — it is
 * the same shape as a project board that has not arrived.
 */
export function mergeBoards(
  boards: readonly BoardView[],
  project: ProjectDto,
): BoardView | null {
  if (boards.length === 0) return null;
  const columns: BoardView["columns"] = [];
  const index = new Map<string, number>();
  for (const board of boards) {
    for (const column of board.columns) {
      const at = index.get(column.state.id);
      if (at === undefined) {
        index.set(column.state.id, columns.length);
        columns.push({ state: column.state, rows: [...column.rows] });
      } else {
        columns[at]!.rows.push(...column.rows);
      }
    }
  }
  return { schema_version: boards[0]!.schema_version, project, columns };
}

/**
 * The stand-in project a team's board reports itself as.
 *
 * Carries the team's own id and key so a caching layer keyed on
 * `board.project.id` cannot confuse two teams — or a team with a real project.
 */
export function teamAsProject(team: TeamDto): ProjectDto {
  return { id: team.id, name: team.name, key: team.key, color: "gray" };
}
