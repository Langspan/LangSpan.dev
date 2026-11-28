import { readFile } from 'fs';

export interface Options {
  port: number;
}

export class Server {
  serve(options: Options): void {
    bind(options.port);
  }
}

export const PORT = 8080;

function bind(port: number): number {
  return port;
}
