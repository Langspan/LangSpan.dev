package server

import "fmt"

type Server struct {
	Port int
}

type Listener interface {
	Listen() error
}

func (s *Server) Serve() error {
	return bind(s.Port)
}

func bind(port int) error {
	fmt.Println(port)
	return nil
}
