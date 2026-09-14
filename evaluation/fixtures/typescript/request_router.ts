export class RequestRouter {
  routeCount = 0;
  routeRequest = (path: string): string => {
    const normalizedPath = path.trim();
    this.routeCount += 1;
    return normalizedPath;
  };
}
export function parseRoute(path: string): string;
export function parseRoute(path: string | undefined): string {
  return path || "/";
}
export const forwardRoute = (path: string) => parseRoute(path);
