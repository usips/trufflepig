import { parseRoute } from "./request_router.js";
export function readRoute(input) {
  return parseRoute(input);
}
export function unknownReceiver(receiver) {
  return receiver.parseRoute("/");
}
