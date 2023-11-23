#!/bin/bash


spawn docker-compose run --name foo teleforward init

expect "wait for input"
send "123123\n"

interact