export interface Forward {
  name: string;
  local: number;
  remote: string;
}

export interface Config {
  host: string;
  user: string;
  proxyCommand: string;
  closeToTray: boolean;
  forwards: Forward[];
}

export interface StatusPayload {
  text: string;
  /** muted / amber / accent / red */
  kind: string;
}

export interface ExitPayload {
  port: number;
  /** idle / testing / ok / fail */
  state: string;
  text: string;
}

export interface Snapshot {
  config: Config;
  status: StatusPayload;
  wantRun: boolean;
  connected: boolean;
  logs: string[];
  exits: ExitPayload[];
  autostart: boolean;
}
