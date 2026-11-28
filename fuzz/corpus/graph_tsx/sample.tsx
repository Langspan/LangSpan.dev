import React from 'react';

export function Login({ name }: { name: string }) {
  const label = format(name);
  return <button onClick={() => submit(label)}>{label}</button>;
}

const format = (s: string) => s.trim();
